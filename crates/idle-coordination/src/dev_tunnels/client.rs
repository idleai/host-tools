use async_trait::async_trait;
use tokio_util::sync::CancellationToken;
use tunnels::{
    connections::{ClientRelayHandle, RelayTunnelClient},
    contracts::{
        LocalNetworkTunnelEndpoint, TunnelConnectionMode, TunnelEndpoint, TunnelRelayTunnelEndpoint,
    },
    management::{TunnelManagementClient, new_tunnel_management},
};

use crate::{
    Error, Result,
    invitation::{Invitation, RelayEndpoint, Secret, token_expiration},
    transport::{BoxStream, ClientTransport, bounded},
};

use super::{CLOSE_TIMEOUT, DevTunnels, REQUEST_TIMEOUT, safe_http};

pub(super) async fn resolve(
    owner: &DevTunnels,
    invitation: &Invitation,
    cancel: &CancellationToken,
) -> Result<Invitation> {
    let mut replacement = invitation.clone();
    replacement.endpoint = resolve_endpoint(
        owner,
        &invitation.endpoint,
        &invitation.connect_token,
        cancel,
    )
    .await?;
    Ok(replacement)
}

pub(super) async fn resolve_endpoint(
    owner: &DevTunnels,
    route: &RelayEndpoint,
    token: &Secret,
    cancel: &CancellationToken,
) -> Result<RelayEndpoint> {
    route.validate()?;
    if token_expiration(token)? <= owner.clock.now_ms()? {
        return Err(Error::Expired);
    }
    let mut builder = new_tunnel_management("idle-coordination/0.1");
    let _builder = builder
        .client(owner.http.clone())
        .authorization(tunnels::management::Authorization::Tunnel(token.0.clone()));
    let management: TunnelManagementClient = builder.into();
    let locator = tunnels::management::TunnelLocator::ID {
        cluster: route.cluster_id.clone(),
        id: route.tunnel_id.clone(),
    };
    let options = tunnels::management::TunnelRequestOptions {
        include_ports: true,
        ..Default::default()
    };
    let tunnel = bounded(cancel, REQUEST_TIMEOUT, async {
        management
            .get_tunnel(&locator, &options)
            .await
            .map_err(safe_http)
    })
    .await?;
    if tunnel.tunnel_id.as_ref() != Some(&route.tunnel_id)
        || tunnel.cluster_id.as_ref() != Some(&route.cluster_id)
        || !tunnel
            .ports
            .iter()
            .any(|port| port.port_number == owner.port)
    {
        return Err(Error::Forbidden);
    }
    let endpoints: Vec<_> = tunnel
        .endpoints
        .iter()
        .filter(|endpoint| matches!(endpoint.connection_mode, TunnelConnectionMode::TunnelRelay))
        .collect();
    let endpoint = endpoints
        .iter()
        .find(|endpoint| endpoint.host_id == route.host_id)
        .copied()
        .or_else(|| {
            if endpoints.len() == 1 {
                endpoints.first().copied()
            } else {
                None
            }
        })
        .ok_or(Error::Transport)?;
    let mut replacement = route.clone();
    replacement.host_id.clone_from(&endpoint.host_id);
    replacement
        .host_public_keys
        .clone_from(&endpoint.host_public_keys);
    replacement.client_relay_uri = endpoint
        .tunnel_relay_tunnel_endpoint
        .client_relay_uri
        .clone()
        .ok_or(Error::Invalid)?;
    replacement.validate()?;
    Ok(replacement)
}

pub(super) async fn connect(
    owner: &DevTunnels,
    invitation: &Invitation,
    cancel: &CancellationToken,
) -> Result<Box<dyn ClientTransport>> {
    connect_endpoint(
        owner,
        &invitation.endpoint,
        &invitation.connect_token,
        cancel,
    )
    .await
}

pub(super) async fn connect_endpoint(
    owner: &DevTunnels,
    route: &RelayEndpoint,
    token: &Secret,
    cancel: &CancellationToken,
) -> Result<Box<dyn ClientTransport>> {
    route.validate()?;
    if token_expiration(token)? <= owner.clock.now_ms()? {
        return Err(Error::Expired);
    }
    let endpoint = TunnelEndpoint {
        id: None,
        connection_mode: TunnelConnectionMode::TunnelRelay,
        host_id: route.host_id.clone(),
        host_public_keys: route.host_public_keys.clone(),
        port_uri_format: None,
        tunnel_uri: None,
        port_ssh_command_format: None,
        tunnel_ssh_command: None,
        ssh_gateway_public_key: None,
        local_network_tunnel_endpoint: LocalNetworkTunnelEndpoint::default(),
        tunnel_relay_tunnel_endpoint: TunnelRelayTunnelEndpoint {
            host_relay_uri: None,
            client_relay_uri: Some(route.client_relay_uri.clone()),
        },
    };
    // The client has only the connect grant; it cannot fall back to owner credentials.
    let management: TunnelManagementClient = new_tunnel_management("idle-coordination/0.1").into();
    let client = RelayTunnelClient::new(management);
    let handle = bounded(cancel, REQUEST_TIMEOUT, async {
        client
            .connect(&endpoint, &token.0)
            .await
            .map_err(|_error| Error::Transport)
    })
    .await?;
    let mut connection = SdkClient {
        handle: Some(handle),
        stream: None,
    };
    let handle = connection.handle.as_ref().ok_or(Error::Transport)?;
    let stream = bounded(cancel, REQUEST_TIMEOUT, async {
        handle
            .connect_to_port(owner.port)
            .await
            .map_err(|_error| Error::Transport)
    })
    .await;
    match stream {
        Ok(stream) => {
            connection.stream = Some(Box::new(stream.into_rw()));
            Ok(Box::new(connection))
        }
        Err(error) => {
            let _closed = connection.close().await;
            Err(error)
        }
    }
}

struct SdkClient {
    handle: Option<ClientRelayHandle>,
    stream: Option<BoxStream>,
}

impl std::fmt::Debug for SdkClient {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SdkClient")
            .field("connected", &self.handle.is_some())
            .finish_non_exhaustive()
    }
}

#[async_trait]
impl ClientTransport for SdkClient {
    fn take_stream(&mut self) -> Result<BoxStream> {
        self.stream.take().ok_or(Error::Transport)
    }

    async fn close(&mut self) -> Result<()> {
        let _stream = self.stream.take();
        if let Some(handle) = self.handle.take() {
            bounded(&CancellationToken::new(), CLOSE_TIMEOUT, async {
                super::close_result(handle.close().await)
            })
            .await?;
        }
        Ok(())
    }
}

impl Drop for SdkClient {
    fn drop(&mut self) {
        if let Some(handle) = self.handle.take()
            && let Ok(runtime) = tokio::runtime::Handle::try_current()
        {
            let _cleanup = runtime.spawn(async move {
                let _closed = tokio::time::timeout(CLOSE_TIMEOUT, handle.close()).await;
            });
        }
    }
}
