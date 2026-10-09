use async_trait::async_trait;
use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    sync::mpsc,
    task::JoinSet,
};
use tokio_util::sync::CancellationToken;
use tunnels::{
    connections::{ForwardedPortConnection, RelayHandle, RelayTunnelHost},
    management::TunnelRequestOptions,
};

use crate::{
    Error, Result,
    invitation::{HostLease, Invitation, RelayEndpoint, token_expiration},
    transport::{
        BoxStream, ClientTransport, CloseMode, HostTransport, RelayDescriptor, RelayProvider,
        bounded,
    },
};

use super::{CLOSE_TIMEOUT, DevTunnels, REQUEST_TIMEOUT, locator, owned, safe_http, token};

struct SdkHost {
    owner: DevTunnels,
    lease: HostLease,
    host: Option<RelayTunnelHost>,
    handle: Option<RelayHandle>,
    incoming: mpsc::UnboundedReceiver<ForwardedPortConnection>,
    endpoint: RelayEndpoint,
    endpoint_id: String,
    expires_at: u64,
    forwarding: JoinSet<Result<()>>,
    forwarding_cancel: CancellationToken,
}

impl std::fmt::Debug for SdkHost {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SdkHost")
            .field("lease", &self.lease)
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl RelayProvider for DevTunnels {
    fn import_cleanup(&self, markers: &[String]) -> Result<()> {
        if markers.len() > 64
            || markers
                .iter()
                .any(|marker| !crate::invitation::valid_marker(marker))
        {
            return Err(Error::Invalid);
        }
        for marker in markers {
            self.journal.remember(&super::Entry {
                marker: marker.clone(),
                lease: None,
                created_at: self.clock.now_ms()?,
            })?;
        }
        Ok(())
    }

    async fn host(
        &self,
        previous: Option<&HostLease>,
        cancel: &CancellationToken,
    ) -> Result<Box<dyn HostTransport>> {
        let (tunnel, lease) = self.acquire(previous, cancel).await?;
        let result = self.start_host(&tunnel, lease.clone(), cancel).await;
        if result.is_err() && previous.is_none() {
            // The marker remains durable if removal is unavailable or uncertain.
            let _cleanup = self.remove_owned(&lease, &CancellationToken::new()).await;
        }
        result
    }

    async fn connect(
        &self,
        invitation: &Invitation,
        cancel: &CancellationToken,
    ) -> Result<Box<dyn ClientTransport>> {
        super::client::connect(self, invitation, cancel).await
    }

    async fn resolve(
        &self,
        invitation: &Invitation,
        cancel: &CancellationToken,
    ) -> Result<Invitation> {
        super::client::resolve(self, invitation, cancel).await
    }

    async fn remove(&self, lease: &HostLease, cancel: &CancellationToken) -> Result<()> {
        self.remove_owned(lease, cancel).await
    }

    async fn cleanup(
        &self,
        retained: Option<&HostLease>,
        cancel: &CancellationToken,
    ) -> Result<()> {
        self.sweep(retained, cancel).await
    }
}

impl DevTunnels {
    async fn start_host(
        &self,
        tunnel: &tunnels::contracts::Tunnel,
        lease: HostLease,
        cancel: &CancellationToken,
    ) -> Result<Box<dyn HostTransport>> {
        let host_token = token(tunnel, "host")?;
        let expires_at = token_expiration(&host_token)?;
        if expires_at <= self.clock.now_ms()?.saturating_add(60_000) {
            return Err(Error::Expired);
        }
        let mut host = RelayTunnelHost::new(locator(&lease), self.management(cancel));
        let incoming = bounded(cancel, REQUEST_TIMEOUT, async {
            host.add_port_raw(&self.port())
                .await
                .map_err(|_error| Error::Transport)
        })
        .await?;
        let handle = bounded(cancel, REQUEST_TIMEOUT, async {
            host.connect(&host_token.0)
                .await
                .map_err(|_error| Error::Transport)
        })
        .await?;
        let endpoint = handle.endpoint();
        let endpoint_id = endpoint
            .id
            .clone()
            .unwrap_or_else(|| format!("{}-relay", endpoint.host_id));
        let endpoint = RelayEndpoint {
            tunnel_id: lease.tunnel_id.clone(),
            cluster_id: lease.cluster_id.clone(),
            host_id: endpoint.host_id.clone(),
            client_relay_uri: endpoint
                .tunnel_relay_tunnel_endpoint
                .client_relay_uri
                .clone()
                .unwrap_or_default(),
            host_public_keys: endpoint.host_public_keys.clone(),
        };
        let mut result = SdkHost {
            owner: self.clone(),
            lease,
            host: Some(host),
            handle: Some(handle),
            incoming,
            endpoint,
            endpoint_id,
            expires_at,
            forwarding: JoinSet::new(),
            forwarding_cancel: CancellationToken::new(),
        };
        if let Err(error) = result.endpoint.validate() {
            let _closed = result.close(CloseMode::Suspend).await;
            return Err(error);
        }
        Ok(Box::new(result))
    }
}

#[async_trait]
impl HostTransport for SdkHost {
    fn lease(&self) -> HostLease {
        self.lease.clone()
    }
    fn expires_at(&self) -> u64 {
        self.expires_at
    }

    async fn descriptor(&mut self, cancel: &CancellationToken) -> Result<RelayDescriptor> {
        if self.handle.is_none() {
            return Err(Error::Transport);
        }
        let management = self.owner.management(cancel);
        let options = TunnelRequestOptions {
            include_ports: true,
            token_scopes: vec!["connect".into()],
            ..Default::default()
        };
        let tunnel = bounded(cancel, REQUEST_TIMEOUT, async {
            management
                .get_tunnel(&locator(&self.lease), &options)
                .await
                .map_err(safe_http)
        })
        .await?;
        owned(&tunnel, &self.lease)?;
        let connect_token = token(&tunnel, "connect")?;
        let now = self.owner.clock.now_ms()?;
        let expiration = token_expiration(&connect_token)?;
        if expiration <= now {
            return Err(Error::Expired);
        }
        Ok(RelayDescriptor {
            endpoint: self.endpoint.clone(),
            connect_token,
            expires_at: expiration.min(now.saturating_add(3_600_000)),
        })
    }

    async fn accept(&mut self, cancel: &CancellationToken) -> Result<BoxStream> {
        loop {
            let handle = self.handle.as_mut().ok_or(Error::Transport)?;
            let incoming = tokio::select! {
                biased;
                () = cancel.cancelled() => return Err(Error::Cancelled),
                _closed = handle => None,
                _completed = self.forwarding.join_next(), if !self.forwarding.is_empty() => continue,
                incoming = self.incoming.recv() => incoming,
            };
            if let Some(connection) = incoming {
                if self.forwarding.len() >= 8 {
                    tokio::time::timeout(CLOSE_TIMEOUT, connection.close())
                        .await
                        .map_err(|_error| Error::Timeout)?;
                    continue;
                }
                // Retain the SDK connection until the native consumer drops its
                // bounded stream. The SDK's into_rw() shutdown is a no-op.
                let (local, remote) = tokio::io::duplex(64 * 1024);
                let _forwarder = self.forwarding.spawn(forward(
                    connection,
                    local,
                    self.forwarding_cancel.child_token(),
                ));
                return Ok(Box::new(remote));
            }
            // A completed SDK handle must not be polled a second time by close().
            let _completed = self.handle.take();
            return Err(Error::Transport);
        }
    }

    async fn close(&mut self, mode: CloseMode) -> Result<()> {
        self.incoming.close();
        self.forwarding_cancel.cancel();
        let cancel = CancellationToken::new();
        let mut failed = false;
        while let Some(completed) = self.forwarding.join_next().await {
            failed |= completed.is_err();
        }
        let drained = tokio::time::timeout(CLOSE_TIMEOUT, async {
            while let Ok(connection) = self.incoming.try_recv() {
                connection.close().await;
            }
        })
        .await;
        failed |= drained.is_err();
        if let Some(handle) = self.handle.take() {
            failed = bounded(&cancel, CLOSE_TIMEOUT, async {
                super::close_result(handle.close().await)
            })
            .await
            .is_err();
        }
        let _host = self.host.take();
        let management = self.owner.management(&cancel);
        let unregister = bounded(&cancel, CLOSE_TIMEOUT, async {
            match management
                .delete_tunnel_endpoints(
                    &locator(&self.lease),
                    &self.endpoint_id,
                    &TunnelRequestOptions::default(),
                )
                .await
            {
                Ok(_removed) => Ok(()),
                Err(error) if super::not_found(&error) => Ok(()),
                Err(error) => Err(safe_http(error)),
            }
        })
        .await;
        failed |= unregister.is_err();
        if mode == CloseMode::Remove {
            self.owner.remove_owned(&self.lease, &cancel).await?;
        }
        if failed {
            Err(Error::Transport)
        } else {
            Ok(())
        }
    }
}

impl Drop for SdkHost {
    fn drop(&mut self) {
        self.forwarding_cancel.cancel();
        if let Some(handle) = self.handle.take()
            && let Ok(runtime) = tokio::runtime::Handle::try_current()
        {
            let _cleanup = runtime.spawn(async move {
                let _closed = tokio::time::timeout(CLOSE_TIMEOUT, handle.close()).await;
            });
        }
    }
}

async fn forward(
    mut connection: ForwardedPortConnection,
    mut stream: tokio::io::DuplexStream,
    cancel: CancellationToken,
) -> Result<()> {
    let result = async {
        let mut buffer = vec![0_u8; 64 * 1024];
        loop {
            tokio::select! {
                biased;
                () = cancel.cancelled() => return Ok(()),
                bytes = connection.recv() => {
                    let Some(bytes) = bytes else { return Ok(()); };
                    bounded(&cancel, REQUEST_TIMEOUT, async { stream.write_all(&bytes).await.map_err(|_error| Error::Transport) }).await?;
                }
                read = stream.read(&mut buffer) => {
                    let count = read.map_err(|_error| Error::Transport)?;
                    if count == 0 { return Ok(()); }
                    let bytes = buffer.get(..count).ok_or(Error::Invalid)?;
                    bounded(&cancel, REQUEST_TIMEOUT, async { connection.send(bytes).await.map_err(|_error| Error::Transport) }).await?;
                }
            }
        }
    }.await;
    let closed = tokio::time::timeout(CLOSE_TIMEOUT, connection.close())
        .await
        .map_err(|_error| Error::Timeout);
    closed.and(result)
}
