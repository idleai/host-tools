use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;

use crate::{
    Error, Result,
    invitation::{Invitation, Secret},
    transport::{Credentials, bounded},
};

/// A credential purpose selected by the native adapter, never by remote peers.
#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CredentialPurpose {
    /// Owner-only Dev Tunnels management.
    Management,
    /// Explicitly approved GitHub repository discovery.
    Discovery,
}

/// Private native-to-host request, multiplexed with ordinary service responses.
#[derive(Debug, Serialize)]
pub struct CredentialRequest {
    /// Connection-local correlation ID.
    pub id: String,
    /// Required host authorization.
    pub purpose: CredentialPurpose,
}

/// Private host reply. Tokens never enter status, diagnostics or configuration.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct CredentialReply {
    /// Original request ID.
    pub id: String,
    /// A fresh token, or `None` when authorization is unavailable.
    pub token: Option<Secret>,
}

#[derive(Debug, Default)]
struct Pending {
    sequence: u64,
    calls: BTreeMap<String, oneshot::Sender<Result<Secret>>>,
}

#[derive(Debug)]
pub(super) struct HostCredentials {
    pending: Mutex<Pending>,
    sender: mpsc::Sender<CredentialRequest>,
    closed: CancellationToken,
}

struct Registration<'a> {
    pending: &'a Mutex<Pending>,
    id: String,
}

impl Drop for Registration<'_> {
    fn drop(&mut self) {
        if let Ok(mut pending) = self.pending.lock() {
            let _removed = pending.calls.remove(&self.id);
        }
    }
}

impl HostCredentials {
    pub(super) fn new() -> (Arc<Self>, mpsc::Receiver<CredentialRequest>) {
        let (sender, receiver) = mpsc::channel(8);
        (
            Arc::new(Self {
                pending: Mutex::default(),
                sender,
                closed: CancellationToken::new(),
            }),
            receiver,
        )
    }

    pub(super) fn reply(&self, reply: CredentialReply) -> Result<()> {
        if reply.id.len() > 256
            || reply
                .token
                .as_ref()
                .is_some_and(|token| token.0.is_empty() || token.0.len() > 65_536)
        {
            return Err(Error::Invalid);
        }
        if let Some(sender) = self
            .pending
            .lock()
            .map_err(|_error| Error::Storage)?
            .calls
            .remove(&reply.id)
        {
            let _sent = sender.send(reply.token.ok_or(Error::Forbidden));
        }
        Ok(())
    }

    pub(super) fn close(&self) {
        self.closed.cancel();
    }

    async fn request(
        &self,
        purpose: CredentialPurpose,
        cancel: &CancellationToken,
    ) -> Result<Secret> {
        let (id, receiver) = {
            let mut pending = self.pending.lock().map_err(|_error| Error::Storage)?;
            if pending.calls.len() >= 8 {
                return Err(Error::Busy);
            }
            pending.sequence = pending.sequence.checked_add(1).ok_or(Error::Invalid)?;
            let id = pending.sequence.to_string();
            let (sender, receiver) = oneshot::channel();
            let _previous = pending.calls.insert(id.clone(), sender);
            (id, receiver)
        };
        let _registration = Registration {
            pending: &self.pending,
            id: id.clone(),
        };
        let request = CredentialRequest {
            id: id.clone(),
            purpose,
        };
        async {
            self.sender
                .try_send(request)
                .map_err(|_error| Error::Busy)?;
            bounded(cancel, Duration::from_secs(20), async {
                tokio::select! {
                    biased;
                    () = self.closed.cancelled() => Err(Error::Cancelled),
                    reply = receiver => reply.map_err(|_error| Error::Transport)?,
                }
            })
            .await
        }
        .await
    }
}

#[async_trait]
impl Credentials for HostCredentials {
    async fn management(&self, cancel: &CancellationToken) -> Result<Secret> {
        self.request(CredentialPurpose::Management, cancel).await
    }

    async fn discovery(&self, cancel: &CancellationToken) -> Result<Secret> {
        self.request(CredentialPurpose::Discovery, cancel).await
    }

    async fn renew(
        &self,
        _invitation: &Invitation,
        _cancel: &CancellationToken,
    ) -> Result<Option<Invitation>> {
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn callbacks_are_correlated_fresh_and_redacted() {
        let (credentials, mut events) = HostCredentials::new();
        for purpose in [CredentialPurpose::Management, CredentialPurpose::Discovery] {
            let provider = credentials.clone();
            let call =
                tokio::spawn(
                    async move { provider.request(purpose, &CancellationToken::new()).await },
                );
            let event = events.recv().await.expect("credential request");
            assert!(
                format!("{event:?}").contains(&format!("{purpose:?}")),
                "purpose must reach the host"
            );
            let token = Secret(format!("private-{}", event.id));
            let reply = CredentialReply {
                id: event.id,
                token: Some(token.clone()),
            };
            assert!(
                !format!("{reply:?}").contains(&token.0),
                "debug output must redact bearer data"
            );
            credentials.reply(reply).expect("host reply");
            assert_eq!(
                call.await.expect("callback completes"),
                Ok(token),
                "only this request gets the reply"
            );
        }
        assert!(
            credentials
                .pending
                .lock()
                .expect("pending map")
                .calls
                .is_empty(),
            "completed callbacks release correlation slots"
        );
    }

    #[tokio::test]
    async fn cancelled_dropped_and_closed_callbacks_release_all_slots() {
        let (credentials, mut events) = HostCredentials::new();
        for _index in 0..12 {
            let provider = credentials.clone();
            let call =
                tokio::spawn(async move { provider.management(&CancellationToken::new()).await });
            let event = events.recv().await.expect("credential request");
            call.abort();
            assert!(call.await.is_err(), "dropped work must finish");
            credentials
                .reply(CredentialReply {
                    id: event.id,
                    token: Some(Secret("stale".into())),
                })
                .expect("late callback ignored");
            assert!(
                credentials
                    .pending
                    .lock()
                    .expect("pending map")
                    .calls
                    .is_empty(),
                "dropped futures cannot exhaust callback slots"
            );
        }
        let provider = credentials.clone();
        let call =
            tokio::spawn(async move { provider.management(&CancellationToken::new()).await });
        let _event = events.recv().await.expect("credential request");
        credentials.close();
        assert_eq!(
            call.await.expect("closed callback"),
            Err(Error::Cancelled),
            "EOF must retire pending credentials"
        );
    }

    #[tokio::test]
    async fn denied_and_oversized_tokens_never_authorize_management() {
        let (credentials, mut events) = HostCredentials::new();
        let provider = credentials.clone();
        let call =
            tokio::spawn(async move { provider.management(&CancellationToken::new()).await });
        let event = events.recv().await.expect("credential request");
        assert_eq!(
            credentials.reply(CredentialReply {
                id: event.id.clone(),
                token: Some(Secret("x".repeat(65_537)))
            }),
            Err(Error::Invalid),
            "oversized credentials are rejected"
        );
        credentials
            .reply(CredentialReply {
                id: event.id,
                token: None,
            })
            .expect("fixed denial");
        assert_eq!(
            call.await.expect("denied callback"),
            Err(Error::Forbidden),
            "provider diagnostics do not cross the pipe"
        );
    }
}
