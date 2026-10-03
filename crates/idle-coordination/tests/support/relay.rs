use std::{
    collections::BTreeMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
};

use async_trait::async_trait;
use idle_coordination::{
    Error, Result,
    clock::Clock as _,
    engine::Engine,
    invitation::{HostLease, Invitation, RelayEndpoint, Secret},
    peer::{PeerCoordinator, PeerOptions},
    persistence::Persistence,
    transport::{
        BoxStream, ClientTransport, CloseMode, Credentials, HostTransport, RelayDescriptor,
        RelayProvider,
    },
};
use tokio::{io::DuplexStream, sync::mpsc};
use tokio_util::sync::CancellationToken;

use super::{TestClock, token};

#[derive(Debug, Default)]
pub(crate) struct Credential {
    pub renewal: Mutex<Option<Invitation>>,
    pub renewals: AtomicU64,
    pub management_calls: AtomicU64,
}

#[async_trait]
impl Credentials for Credential {
    async fn management(&self, _cancel: &CancellationToken) -> Result<Secret> {
        let _calls = self.management_calls.fetch_add(1, Ordering::SeqCst);
        Ok(Secret("private-owner-token".into()))
    }
    async fn renew(
        &self,
        _invitation: &Invitation,
        _cancel: &CancellationToken,
    ) -> Result<Option<Invitation>> {
        let _calls = self.renewals.fetch_add(1, Ordering::SeqCst);
        Ok(self
            .renewal
            .lock()
            .map_err(|_error| Error::Storage)?
            .clone())
    }
}

#[derive(Debug)]
pub(crate) struct Relay {
    pub clock: Arc<TestClock>,
    pub hosts: Mutex<BTreeMap<String, mpsc::Sender<DuplexStream>>>,
    pub resources: Mutex<BTreeMap<String, HostLease>>,
    pub host_attempts: AtomicU64,
    pub connect_attempts: AtomicU64,
    pub active_clients: Arc<AtomicU64>,
    pub fail_connect: AtomicBool,
    pub fail_remove: AtomicBool,
    pub fail_close: Arc<AtomicBool>,
    pub block_host: AtomicBool,
}

impl Relay {
    pub(crate) fn new(clock: Arc<TestClock>) -> Arc<Self> {
        Arc::new(Self {
            clock,
            hosts: Mutex::default(),
            resources: Mutex::default(),
            host_attempts: AtomicU64::new(0),
            connect_attempts: AtomicU64::new(0),
            active_clients: Arc::new(AtomicU64::new(0)),
            fail_connect: AtomicBool::new(false),
            fail_remove: AtomicBool::new(false),
            fail_close: Arc::new(AtomicBool::new(false)),
            block_host: AtomicBool::new(false),
        })
    }

    pub(crate) async fn coordinator(
        self: &Arc<Self>,
        engine: Engine,
        storage: Arc<dyn Persistence>,
        credentials: Arc<Credential>,
    ) -> Result<PeerCoordinator> {
        PeerCoordinator::open(PeerOptions {
            engine,
            relay: self.clone(),
            storage,
            credentials,
            clock: self.clock.clone(),
        })
        .await
    }
}

fn endpoint(lease: &HostLease) -> RelayEndpoint {
    RelayEndpoint {
        tunnel_id: lease.tunnel_id.clone(),
        cluster_id: lease.cluster_id.clone(),
        host_id: "test-host".into(),
        client_relay_uri: "wss://test.rel.tunnels.api.visualstudio.com/connect".into(),
        host_public_keys: vec!["YWJj".into()],
    }
}

#[async_trait]
impl RelayProvider for Relay {
    async fn host(
        &self,
        previous: Option<&HostLease>,
        cancel: &CancellationToken,
    ) -> Result<Box<dyn HostTransport>> {
        let attempt = self.host_attempts.fetch_add(1, Ordering::SeqCst);
        if self.block_host.load(Ordering::SeqCst) {
            cancel.cancelled().await;
            return Err(Error::Cancelled);
        }
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        let lease = previous.cloned().unwrap_or_else(|| HostLease {
            marker: format!("idle-relay-{attempt:024x}"),
            tunnel_id: uuid::Uuid::new_v4().to_string(),
            cluster_id: "test".into(),
        });
        let (sender, incoming) = mpsc::channel(8);
        let _old = self
            .hosts
            .lock()
            .map_err(|_error| Error::Storage)?
            .insert(lease.tunnel_id.clone(), sender);
        let _old = self
            .resources
            .lock()
            .map_err(|_error| Error::Storage)?
            .insert(lease.tunnel_id.clone(), lease.clone());
        Ok(Box::new(Host {
            lease,
            incoming,
            clock: self.clock.clone(),
            fail_close: self.fail_close.clone(),
        }))
    }

    async fn connect(
        &self,
        invitation: &Invitation,
        cancel: &CancellationToken,
    ) -> Result<Box<dyn ClientTransport>> {
        let _attempt = self.connect_attempts.fetch_add(1, Ordering::SeqCst);
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        if self.fail_connect.load(Ordering::SeqCst) {
            return Err(Error::Transport);
        }
        let sender = self
            .hosts
            .lock()
            .map_err(|_error| Error::Storage)?
            .get(&invitation.endpoint.tunnel_id)
            .cloned()
            .ok_or(Error::Transport)?;
        let (client, server) = tokio::io::duplex(64 * 1024);
        sender
            .send(server)
            .await
            .map_err(|_error| Error::Transport)?;
        let _active = self.active_clients.fetch_add(1, Ordering::SeqCst);
        Ok(Box::new(Client {
            stream: Some(client),
            active: self.active_clients.clone(),
            closed: false,
        }))
    }

    async fn remove(&self, lease: &HostLease, _cancel: &CancellationToken) -> Result<()> {
        if self.fail_remove.load(Ordering::SeqCst) {
            return Err(Error::Transport);
        }
        let mut resources = self.resources.lock().map_err(|_error| Error::Storage)?;
        if resources
            .get(&lease.tunnel_id)
            .is_some_and(|owned| owned != lease)
        {
            return Err(Error::Forbidden);
        }
        let _removed = resources.remove(&lease.tunnel_id);
        let _removed = self
            .hosts
            .lock()
            .map_err(|_error| Error::Storage)?
            .remove(&lease.tunnel_id);
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

#[derive(Debug)]
struct Host {
    lease: HostLease,
    incoming: mpsc::Receiver<DuplexStream>,
    clock: Arc<TestClock>,
    fail_close: Arc<AtomicBool>,
}

#[async_trait]
impl HostTransport for Host {
    fn lease(&self) -> HostLease {
        self.lease.clone()
    }
    fn expires_at(&self) -> u64 {
        self.clock
            .now_ms()
            .unwrap_or_default()
            .saturating_add(3_600_000)
    }
    async fn descriptor(&mut self, _cancel: &CancellationToken) -> Result<RelayDescriptor> {
        let expiry = self.clock.now_ms()?.saturating_add(3_600_000);
        Ok(RelayDescriptor {
            endpoint: endpoint(&self.lease),
            connect_token: token(expiry),
            expires_at: expiry,
        })
    }
    async fn accept(&mut self, cancel: &CancellationToken) -> Result<BoxStream> {
        tokio::select! { () = cancel.cancelled() => Err(Error::Cancelled),
        incoming = self.incoming.recv() => incoming.map(|stream| -> BoxStream { Box::new(stream) }).ok_or(Error::Transport) }
    }
    async fn close(&mut self, _mode: CloseMode) -> Result<()> {
        self.incoming.close();
        while self.incoming.try_recv().is_ok() {}
        if self.fail_close.swap(false, Ordering::SeqCst) {
            Err(Error::Transport)
        } else {
            Ok(())
        }
    }
}

#[derive(Debug)]
struct Client {
    stream: Option<DuplexStream>,
    active: Arc<AtomicU64>,
    closed: bool,
}

#[async_trait]
impl ClientTransport for Client {
    fn take_stream(&mut self) -> Result<BoxStream> {
        Ok(Box::new(self.stream.take().ok_or(Error::Transport)?))
    }
    async fn close(&mut self) -> Result<()> {
        let _stream = self.stream.take();
        self.retire();
        Ok(())
    }
}

impl Client {
    fn retire(&mut self) {
        if !self.closed {
            self.closed = true;
            let _active = self.active.fetch_sub(1, Ordering::SeqCst);
        }
    }
}
impl Drop for Client {
    fn drop(&mut self) {
        self.retire();
    }
}
