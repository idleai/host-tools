//! Microsoft Rust SDK management and encrypted host/client adapters.
//!
//! The SDK revision is pinned in Cargo.toml. Reconnection, bounded lifetimes,
//! consent checks and durable resource cleanup remain with this crate.

mod client;
mod host;
mod journal;

#[cfg(test)]
mod tests;

use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;
use tunnels::{
    contracts::{Tunnel, TunnelPort},
    management::{
        Authorization, AuthorizationProvider, HttpError, TunnelLocator, TunnelManagementClient,
        TunnelRequestOptions, new_tunnel_management,
    },
};

use crate::{
    Error, Result,
    clock::Clock,
    invitation::{HostLease, MULTIPLAYER_PORT, Secret},
    persistence::Persistence,
    transport::{Credentials, bounded},
};

use journal::{Entry, Journal};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const CLOSE_TIMEOUT: Duration = Duration::from_secs(10);

fn close_result<E: std::error::Error + 'static>(result: std::result::Result<(), E>) -> Result<()> {
    // At this pin, disconnect() only enqueues a message. SendError means the
    // session's command receiver has already closed, including a remote restart.
    match result {
        Ok(()) => Ok(()),
        Err(error) => {
            if matches!(
                error
                    .source()
                    .and_then(|source| source.downcast_ref::<russh::Error>()),
                Some(russh::Error::SendError)
            ) {
                Ok(())
            } else {
                Err(Error::Transport)
            }
        }
    }
}

/// Exact upstream revision exercised by the SDK and integration checks.
pub const SDK_REVISION: &str = "bb2a7dbdc56312b01b86be6eb8ce9cda7bb932a2";

/// Dedicated runtime port. History grants never select this port.
pub const RUNTIME_PORT: u16 = 43189;

/// Microsoft's native SDK behind injected owner credentials and private persistence.
#[derive(Clone, Debug)]
pub struct DevTunnels {
    credentials: Arc<dyn Credentials>,
    clock: Arc<dyn Clock>,
    journal: Arc<Journal>,
    http: reqwest::Client,
    port: u16,
}

impl DevTunnels {
    /// Construct an adapter with bounded management requests and no redirects.
    /// Credentials are retrieved per request and never logged or persisted here.
    ///
    /// # Errors
    /// Returns native HTTP/TLS initialization failures.
    pub fn new(
        credentials: Arc<dyn Credentials>,
        storage: Arc<dyn Persistence>,
        clock: Arc<dyn Clock>,
    ) -> Result<Self> {
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(10))
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|_error| Error::Transport)?;
        Ok(Self {
            credentials,
            clock,
            journal: Arc::new(Journal::new(storage)),
            http,
            port: MULTIPLAYER_PORT,
        })
    }

    /// Construct a separate runtime relay with its own private resource journal.
    ///
    /// # Errors
    /// Returns native HTTP/TLS initialization failures.
    pub fn runtime(
        credentials: Arc<dyn Credentials>,
        storage: Arc<dyn Persistence>,
        clock: Arc<dyn Clock>,
    ) -> Result<Self> {
        let mut adapter = Self::new(credentials, storage, clock)?;
        adapter.port = RUNTIME_PORT;
        Ok(adapter)
    }

    /// Connect to the runtime port using only the supplied relay grant.
    /// The daemon separately authenticates and restricts the runtime session.
    ///
    /// # Errors
    /// Rejects expired grants, changed tunnel identities and unavailable routes.
    pub async fn connect_runtime(
        &self,
        descriptor: &crate::transport::RelayDescriptor,
        cancel: &CancellationToken,
    ) -> Result<Box<dyn crate::transport::ClientTransport>> {
        if self.port != RUNTIME_PORT || descriptor.expires_at <= self.clock.now_ms()? {
            return Err(Error::Expired);
        }
        let endpoint = client::resolve_endpoint(
            self,
            &descriptor.endpoint,
            &descriptor.connect_token,
            cancel,
        )
        .await?;
        client::connect_endpoint(self, &endpoint, &descriptor.connect_token, cancel).await
    }

    fn management(&self, cancel: &CancellationToken) -> TunnelManagementClient {
        let mut builder = new_tunnel_management("idle-coordination/0.1");
        let _builder =
            builder
                .client(self.http.clone())
                .authorization_provider(OwnerAuthorization {
                    credentials: self.credentials.clone(),
                    cancel: cancel.clone(),
                });
        builder.into()
    }

    async fn acquire(
        &self,
        previous: Option<&HostLease>,
        cancel: &CancellationToken,
    ) -> Result<(Tunnel, HostLease)> {
        let management = self.management(cancel);
        let options = TunnelRequestOptions {
            include_ports: true,
            token_scopes: vec!["host".into()],
            ..Default::default()
        };
        if let Some(lease) = previous {
            lease.validate()?;
            let tunnel = bounded(cancel, REQUEST_TIMEOUT, async {
                management
                    .get_tunnel(&locator(lease), &options)
                    .await
                    .map_err(safe_http)
            })
            .await?;
            owned(&tunnel, lease)?;
            self.journal.remember(&Entry {
                marker: lease.marker.clone(),
                lease: Some(lease.clone()),
                created_at: self.clock.now_ms()?,
            })?;
            return Ok((tunnel, lease.clone()));
        }
        let marker = format!(
            "idle-relay-{}",
            uuid::Uuid::new_v4()
                .simple()
                .to_string()
                .chars()
                .take(24)
                .collect::<String>()
        );
        let mut entry = Entry {
            marker: marker.clone(),
            lease: None,
            created_at: self.clock.now_ms()?,
        };
        self.journal.remember(&entry)?;
        let tunnel = bounded(cancel, REQUEST_TIMEOUT, async {
            management
                .create_tunnel(
                    Tunnel {
                        labels: vec!["idle-relay".into(), marker.clone()],
                        custom_expiration: Some(86_400),
                        ports: vec![self.port()],
                        ..Default::default()
                    },
                    &options,
                )
                .await
                .map_err(safe_http)
        })
        .await?;
        let lease = HostLease {
            marker,
            tunnel_id: tunnel.tunnel_id.clone().ok_or(Error::Invalid)?,
            cluster_id: tunnel.cluster_id.clone().ok_or(Error::Invalid)?,
        };
        owned(&tunnel, &lease)?;
        entry.lease = Some(lease.clone());
        self.journal.remember(&entry)?;
        Ok((tunnel, lease))
    }

    async fn remove_owned(&self, lease: &HostLease, cancel: &CancellationToken) -> Result<()> {
        lease.validate()?;
        let management = self.management(cancel);
        let options = TunnelRequestOptions::default();
        let lookup = bounded(cancel, REQUEST_TIMEOUT, async {
            match management.get_tunnel(&locator(lease), &options).await {
                Ok(tunnel) => Ok(Some(tunnel)),
                Err(error) if not_found(&error) => Ok(None),
                Err(error) => Err(safe_http(error)),
            }
        })
        .await?;
        if let Some(tunnel) = lookup {
            owned(&tunnel, lease)?;
            bounded(cancel, REQUEST_TIMEOUT, async {
                match management.delete_tunnel(&locator(lease), &options).await {
                    Ok(_deleted) => Ok(()),
                    Err(error) if not_found(&error) => Ok(()),
                    Err(error) => Err(safe_http(error)),
                }
            })
            .await?;
        }
        self.journal.forget(&lease.marker)
    }

    async fn sweep(&self, retained: Option<&HostLease>, cancel: &CancellationToken) -> Result<()> {
        let entries = self.journal.entries()?;
        let mut incomplete = false;
        for entry in entries {
            if let Some(lease) = retained.filter(|lease| lease.marker == entry.marker) {
                if entry
                    .lease
                    .as_ref()
                    .is_some_and(|recorded| recorded != lease)
                {
                    return Err(Error::Conflict);
                }
                continue;
            }
            if let Some(lease) = &entry.lease {
                if self.remove_owned(lease, cancel).await.is_err() {
                    incomplete = true;
                }
                continue;
            }
            // A cancelled create may have committed remotely after losing its
            // response. Keep its marker until found or its full resource TTL passes.
            let management = self.management(cancel);
            let options = TunnelRequestOptions {
                labels: vec![entry.marker.clone()],
                require_all_labels: true,
                limit: 100,
                ..Default::default()
            };
            let tunnels = bounded(cancel, REQUEST_TIMEOUT, async {
                management
                    .list_all_tunnels(&options)
                    .await
                    .map_err(safe_http)
            })
            .await?;
            let target = cleanup_target(&tunnels, &entry.marker)?;
            if let Some(lease) = &target {
                self.remove_owned(lease, cancel).await?;
            }
            if target.is_some()
                || self.clock.now_ms()? >= entry.created_at.saturating_add(172_800_000)
            {
                self.journal.forget(&entry.marker)?;
            } else {
                incomplete = true;
            }
        }
        if incomplete {
            Err(Error::Transport)
        } else {
            Ok(())
        }
    }
}

fn cleanup_target(tunnels: &[Tunnel], marker: &str) -> Result<Option<HostLease>> {
    match tunnels {
        [] => Ok(None),
        [tunnel] => {
            if !tunnel.labels.iter().any(|label| label == marker) {
                return Err(Error::Forbidden);
            }
            let lease = HostLease {
                marker: marker.into(),
                tunnel_id: tunnel.tunnel_id.clone().ok_or(Error::Invalid)?,
                cluster_id: tunnel.cluster_id.clone().ok_or(Error::Invalid)?,
            };
            lease.validate()?;
            Ok(Some(lease))
        }
        _ => Err(Error::Conflict),
    }
}

struct OwnerAuthorization {
    credentials: Arc<dyn Credentials>,
    cancel: CancellationToken,
}

impl AuthorizationProvider for OwnerAuthorization {
    fn get_authorization(
        &self,
    ) -> tunnels::management::BoxFuture<'_, std::result::Result<Authorization, HttpError>> {
        Box::pin(async {
            let token = self
                .credentials
                .management(&self.cancel)
                .await
                .map_err(|_error| {
                    HttpError::AuthorizationError("owner credentials unavailable".into())
                })?;
            Ok(Authorization::Github(token.0))
        })
    }
}

/// Headless credential adapter: read a named environment variable on each request.
/// Guest renewal is unavailable until a host supplies an authenticated grant adapter.
#[derive(Clone, Debug)]
pub struct EnvironmentCredentials {
    /// Name of the private variable containing a GitHub management credential.
    pub variable: String,
}

#[async_trait]
impl Credentials for EnvironmentCredentials {
    async fn management(&self, cancel: &CancellationToken) -> Result<Secret> {
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let token = std::env::var(&self.variable).map_err(|_error| Error::Forbidden)?;
        if token.is_empty() || token.len() > 16_384 {
            return Err(Error::Forbidden);
        }
        Ok(Secret(token))
    }

    async fn renew(
        &self,
        _invitation: &crate::invitation::Invitation,
        cancel: &CancellationToken,
    ) -> Result<Option<crate::invitation::Invitation>> {
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        Ok(None)
    }
}

impl DevTunnels {
    fn port(&self) -> TunnelPort {
        TunnelPort {
            port_number: self.port,
            protocol: Some("auto".into()),
            ..Default::default()
        }
    }
}

fn locator(lease: &HostLease) -> TunnelLocator {
    TunnelLocator::ID {
        cluster: lease.cluster_id.clone(),
        id: lease.tunnel_id.clone(),
    }
}

fn owned(tunnel: &Tunnel, lease: &HostLease) -> Result<()> {
    lease.validate()?;
    if tunnel.tunnel_id.as_deref() != Some(&lease.tunnel_id)
        || tunnel.cluster_id.as_deref() != Some(&lease.cluster_id)
        || !tunnel.labels.contains(&lease.marker)
    {
        return Err(Error::Forbidden);
    }
    Ok(())
}

fn not_found(error: &HttpError) -> bool {
    matches!(error, HttpError::ResponseError(response) if response.status_code.as_u16() == 404)
}

fn safe_http(error: HttpError) -> Error {
    match error {
        HttpError::AuthorizationError(_) => Error::Forbidden,
        HttpError::ResponseError(response)
            if matches!(response.status_code.as_u16(), 401 | 403) =>
        {
            Error::Forbidden
        }
        HttpError::ConnectionError(_) | HttpError::ResponseError(_) => Error::Transport,
    }
}

fn token(tunnel: &Tunnel, scope: &str) -> Result<Secret> {
    tunnel
        .access_tokens
        .as_ref()
        .and_then(|tokens| tokens.get(scope))
        .cloned()
        .map(Secret)
        .ok_or(Error::Forbidden)
}
