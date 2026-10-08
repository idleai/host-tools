//! Injected transports own cloud resources; the coordinator owns sharing policy.

use std::time::Duration;

use async_trait::async_trait;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_util::sync::CancellationToken;

use crate::{
    Error, Result,
    invitation::{HostLease, Invitation, RelayEndpoint, Secret},
};

/// Opaque bidirectional stream. Each caller supplies its port's authentication protocol.
pub trait PeerStream: AsyncRead + AsyncWrite + Send + Unpin {}
impl<T: AsyncRead + AsyncWrite + Send + Unpin> PeerStream for T {}

/// Owned transport stream, without runtime/UI types.
pub type BoxStream = Box<dyn PeerStream>;

/// Privately issued connect descriptor. Debug output redacts its bearer grant.
#[derive(Clone, Debug, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RelayDescriptor {
    /// Public endpoint, including SSH host keys.
    pub endpoint: RelayEndpoint,
    /// Current connect grant issued within the owner's tunnel.
    pub connect_token: Secret,
    /// Fresh invitation acceptance deadline, bounded by token expiration.
    pub expires_at: u64,
}

/// Resource teardown policy, independent from device revocation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CloseMode {
    /// Keep the saved cloud resource for restart.
    Suspend,
    /// Remove the owned cloud resource; retain failed cleanup in a durable journal.
    Remove,
}

/// One hosting attempt. Dropping must release operational resources; `close`
/// additionally awaits cleanup and reports any durable deletion still pending.
#[async_trait]
pub trait HostTransport: std::fmt::Debug + Send {
    /// Exact durable resource created or resumed by this host.
    fn lease(&self) -> HostLease;
    /// Host-token expiry, used for proactive authenticated renewal.
    fn expires_at(&self) -> u64;
    /// Issue a current descriptor for an explicitly approved guest.
    ///
    /// # Errors
    /// Reports unavailable hosting or management access.
    async fn descriptor(&mut self, cancel: &CancellationToken) -> Result<RelayDescriptor>;
    /// Accept one opaque stream, or report that hosting disconnected.
    ///
    /// # Errors
    /// Returns cancellation, transport loss or failed encryption setup.
    async fn accept(&mut self, cancel: &CancellationToken) -> Result<BoxStream>;
    /// Close all host resources; removal failures must remain recoverable.
    ///
    /// # Errors
    /// Reports incomplete teardown or pending cloud cleanup.
    async fn close(&mut self, mode: CloseMode) -> Result<()>;
}

/// One outbound relay attempt, owning its SSH session separately from the stream.
#[async_trait]
pub trait ClientTransport: std::fmt::Debug + Send {
    /// Take the one stream belonging to this attempt.
    ///
    /// # Errors
    /// Fails if the stream was already taken or the attempt closed.
    fn take_stream(&mut self) -> Result<BoxStream>;
    /// Await session/transport disposal.
    ///
    /// # Errors
    /// Reports incomplete teardown without disclosing SDK request details.
    async fn close(&mut self) -> Result<()>;
}

/// Host-injected relay implementation. Creation must journal ownership before I/O.
#[async_trait]
pub trait RelayProvider: std::fmt::Debug + Send + Sync {
    /// Create or resume only the explicitly supplied owner resource.
    ///
    /// # Errors
    /// Returns cancellation, failed credential refresh or unavailable relay service.
    async fn host(
        &self,
        previous: Option<&HostLease>,
        cancel: &CancellationToken,
    ) -> Result<Box<dyn HostTransport>>;
    /// Connect using the exact invitation, without account-wide fallback access.
    ///
    /// # Errors
    /// Rejects expired grants, bad host keys and unavailable endpoints.
    async fn connect(
        &self,
        invitation: &Invitation,
        cancel: &CancellationToken,
    ) -> Result<Box<dyn ClientTransport>>;
    /// Refresh a route within the same tunnel using only its existing connect grant.
    ///
    /// # Errors
    /// Reports unavailable discovery; cannot approve a new device or resource.
    async fn resolve(
        &self,
        invitation: &Invitation,
        cancel: &CancellationToken,
    ) -> Result<Invitation> {
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        Ok(invitation.clone())
    }
    /// Remove an exact owned lease after verifying its marker.
    ///
    /// # Errors
    /// Returns unavailable management or ownership mismatch.
    async fn remove(&self, lease: &HostLease, cancel: &CancellationToken) -> Result<()>;
    /// Durably retain ownership markers transferred by the authenticated host.
    ///
    /// # Errors
    /// Rejects unsupported transfers or invalid markers.
    fn import_cleanup(&self, markers: &[String]) -> Result<()> {
        if markers.is_empty() {
            Ok(())
        } else {
            Err(Error::Invalid)
        }
    }
    /// Retry previously journaled incomplete creates/deletes after service restart.
    ///
    /// # Errors
    /// Keeps uncertain cleanup records on failure.
    async fn cleanup(&self, retained: Option<&HostLease>, cancel: &CancellationToken)
    -> Result<()>;
}

/// Credential retrieval and renewal supplied by the authenticated native host.
#[async_trait]
pub trait Credentials: std::fmt::Debug + Send + Sync {
    /// Fetch a fresh account credential for owner-only Microsoft management calls.
    ///
    /// # Errors
    /// Returns authentication unavailability; never prompts from the library.
    async fn management(&self, cancel: &CancellationToken) -> Result<Secret>;
    /// Fetch the separately approved repository discovery credential.
    ///
    /// # Errors
    /// Returns authentication unavailability without prompting.
    async fn discovery(&self, cancel: &CancellationToken) -> Result<Secret> {
        self.management(cancel).await
    }
    /// Renew only an existing peer grant via an authenticated approved source.
    /// `None` means a fresh invitation is required after expiry.
    ///
    /// # Errors
    /// Reports unavailable renewal; callers never widen the scope on failure.
    async fn renew(
        &self,
        invitation: &Invitation,
        cancel: &CancellationToken,
    ) -> Result<Option<Invitation>>;
}

/// Bound an asynchronous operation by its lifetime and explicit deadline.
pub(crate) async fn bounded<T>(
    cancel: &CancellationToken,
    duration: Duration,
    work: impl Future<Output = Result<T>>,
) -> Result<T> {
    tokio::select! {
        biased;
        () = cancel.cancelled() => Err(Error::Cancelled),
        result = tokio::time::timeout(duration, work) => result.map_err(|_error| Error::Timeout)?,
    }
}
