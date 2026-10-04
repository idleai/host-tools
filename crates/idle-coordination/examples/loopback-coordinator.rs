//! Integration fixture: production coordination and encrypted peer workers over loopback.
//! Never packaged as a host adapter; cloud ownership is exercised by the native SDK tests.

use std::{path::Path, sync::Arc};

use async_trait::async_trait;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use idle_coordination::{
    Error, Result,
    authority::{Authority, Bootstrap, Principal},
    clock::{Clock, SystemClock},
    dev_tunnels::EnvironmentCredentials,
    engine::Engine,
    invitation::{HostLease, Invitation, RelayEndpoint, Secret},
    peer::{PeerCoordinator, PeerOptions},
    persistence::FilePersistence,
    service::{Service, native::Configuration, serve},
    transport::{
        BoxStream, ClientTransport, CloseMode, HostTransport, RelayDescriptor, RelayProvider,
    },
};
use tokio::net::{TcpListener, TcpStream};
use tokio_util::sync::CancellationToken;
use {
    blake3 as _, editchain_sync as _, idle_history as _, idle_protocol as _, reqwest as _,
    russh as _, serde as _, sha2 as _, tempfile as _, thiserror as _, tunnels as _, url as _,
};
use {editchain_core as _, editchain_store as _};

#[derive(Debug)]
struct Relay;

#[derive(Debug)]
struct Host {
    listener: TcpListener,
    lease: HostLease,
    expires: u64,
}

#[derive(Debug)]
struct Client(Option<TcpStream>);

#[async_trait]
impl RelayProvider for Relay {
    async fn host(
        &self,
        previous: Option<&HostLease>,
        cancel: &CancellationToken,
    ) -> Result<Box<dyn HostTransport>> {
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let port = previous
            .map(|lease| port(&lease.tunnel_id))
            .transpose()?
            .unwrap_or(0);
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, port)).await?;
        let lease = previous.cloned().unwrap_or(HostLease {
            marker: format!(
                "idle-relay-{}",
                uuid::Uuid::new_v4()
                    .simple()
                    .to_string()
                    .get(..24)
                    .ok_or(Error::Invalid)?
            ),
            tunnel_id: format!("loopback-{}", listener.local_addr()?.port()),
            cluster_id: "loopback".into(),
        });
        Ok(Box::new(Host {
            listener,
            lease,
            expires: SystemClock.now_ms()?.saturating_add(3_600_000),
        }))
    }

    async fn connect(
        &self,
        invitation: &Invitation,
        cancel: &CancellationToken,
    ) -> Result<Box<dyn ClientTransport>> {
        let socket = tokio::select! {
            () = cancel.cancelled() => return Err(Error::Cancelled),
            socket = TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port(&invitation.endpoint.tunnel_id)?)) => socket?,
        };
        Ok(Box::new(Client(Some(socket))))
    }

    async fn remove(&self, _lease: &HostLease, _cancel: &CancellationToken) -> Result<()> {
        Ok(())
    }
    async fn cleanup(
        &self,
        _retained: Option<&HostLease>,
        _cancel: &CancellationToken,
    ) -> Result<()> {
        Ok(())
    }
}

#[async_trait]
impl HostTransport for Host {
    fn lease(&self) -> HostLease {
        self.lease.clone()
    }
    fn expires_at(&self) -> u64 {
        self.expires
    }
    async fn descriptor(&mut self, cancel: &CancellationToken) -> Result<RelayDescriptor> {
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let claims = URL_SAFE_NO_PAD.encode(serde_json::to_vec(
            &serde_json::json!({"exp": self.expires / 1000 + 3600}),
        )?);
        Ok(RelayDescriptor {
            endpoint: RelayEndpoint {
                tunnel_id: self.lease.tunnel_id.clone(),
                cluster_id: self.lease.cluster_id.clone(),
                host_id: "fixture".into(),
                client_relay_uri: "wss://fixture.rel.tunnels.api.visualstudio.com/fixture".into(),
                host_public_keys: vec!["Zml4dHVyZQ==".into()],
            },
            connect_token: Secret(format!("e30.{claims}.fixture")),
            expires_at: self.expires,
        })
    }
    async fn accept(&mut self, cancel: &CancellationToken) -> Result<BoxStream> {
        tokio::select! {
            () = cancel.cancelled() => Err(Error::Cancelled),
            socket = self.listener.accept() => { let (socket, _address) = socket?; Ok(Box::new(socket)) }
        }
    }
    async fn close(&mut self, _mode: CloseMode) -> Result<()> {
        Ok(())
    }
}

#[async_trait]
impl ClientTransport for Client {
    fn take_stream(&mut self) -> Result<BoxStream> {
        Ok(Box::new(self.0.take().ok_or(Error::Invalid)?))
    }
    async fn close(&mut self) -> Result<()> {
        self.0 = None;
        Ok(())
    }
}

fn port(tunnel: &str) -> Result<u16> {
    tunnel
        .strip_prefix("loopback-")
        .ok_or(Error::Invalid)?
        .parse()
        .map_err(|_error| Error::Invalid)
}

async fn open(path: &Path) -> Result<Service> {
    let configuration: Configuration = serde_json::from_slice(&std::fs::read(path)?)?;
    let storage = Arc::new(FilePersistence::open(configuration.state_directory)?);
    let clock = Arc::new(SystemClock);
    let principal = Principal {
        contributor: configuration.contributor,
        runtime: None,
    };
    let authority = Authority::open(
        storage.clone(),
        clock.clone(),
        Some(Bootstrap {
            workspace: configuration.workspace,
            owner: principal.contributor.contributor_id.clone(),
        }),
    )?;
    let engine = Engine {
        chain: configuration.chain_directory,
        device_directory: configuration.device_directory,
    };
    let peers = PeerCoordinator::open(PeerOptions {
        engine: engine.clone(),
        storage,
        clock: clock.clone(),
        relay: Arc::new(Relay),
        credentials: Arc::new(EnvironmentCredentials {
            variable: "UNUSED_FIXTURE_TOKEN".into(),
        }),
    })
    .await?;
    Ok(Service {
        authority,
        peers,
        principal,
        engine,
        clock,
        directory: None,
        adoption: None,
    })
}

#[tokio::main]
async fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    if args.next().as_deref() != Some("--config") {
        return Err(Error::Invalid);
    }
    let path = args.next().ok_or(Error::Invalid)?;
    let mut service = open(Path::new(&path)).await?;
    serve(
        &mut service,
        tokio::io::stdin(),
        tokio::io::stdout(),
        &CancellationToken::new(),
    )
    .await
}
