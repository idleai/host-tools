use std::{sync::Arc, time::Duration};

use idle_history::connection::ConnectionStatus;
use tokio::{
    sync::{mpsc, oneshot},
    task::JoinSet,
};
use tokio_util::sync::CancellationToken;

use crate::{
    Error, Result,
    invitation::{Invitation, token_expiration},
    transport::{CloseMode, RelayDescriptor, bounded},
};

use super::{bridge::Bridge, shared::Shared};

#[derive(Debug)]
pub(super) enum HostRequest {
    Descriptor(oneshot::Sender<Result<RelayDescriptor>>),
}

pub(super) async fn host_loop(
    shared: Arc<Shared>,
    generation: u32,
    cancel: CancellationToken,
    mut commands: mpsc::Receiver<HostRequest>,
) -> Result<()> {
    let result = hosting(&shared, generation, &cancel, &mut commands).await;
    if let Err(error) = result {
        shared.failure(generation, error);
        shared.access(|state| {
            if state.current(generation) && error != Error::Cancelled {
                let _updated = state
                    .host
                    .update(state.host.generation(), ConnectionStatus::Failed);
            }
            Ok(())
        })?;
    }
    while let Ok(HostRequest::Descriptor(reply)) = commands.try_recv() {
        let _sent = reply.send(Err(Error::Transport));
    }
    result
}

async fn hosting(
    shared: &Arc<Shared>,
    generation: u32,
    cancel: &CancellationToken,
    commands: &mut mpsc::Receiver<HostRequest>,
) -> Result<()> {
    while shared.current(generation) && !cancel.is_cancelled() {
        let (previous, attempt) = shared.access(|state| {
            let attempt = state.host.begin().ok_or(Error::Invalid)?;
            Ok((
                state.saved.as_ref().and_then(|saved| saved.host.clone()),
                attempt,
            ))
        })?;
        let mut host = match bounded(
            cancel,
            Duration::from_mins(1),
            shared.relay.host(previous.as_ref(), cancel),
        )
        .await
        {
            Ok(host) => host,
            Err(error) => {
                shared.failure(generation, error);
                if previous.is_none()
                    || matches!(
                        error,
                        Error::Cancelled | Error::Forbidden | Error::Invalid | Error::Version
                    )
                {
                    return Err(error);
                }
                host_backoff(shared, attempt, cancel).await?;
                continue;
            }
        };
        if let Err(error) = shared.save_host(generation, host.lease()) {
            let mode = if previous.is_none() {
                CloseMode::Remove
            } else {
                CloseMode::Suspend
            };
            let _closed = host.close(mode).await;
            return Err(error);
        }
        shared.access(|state| {
            let _updated = state.host.update(attempt, ConnectionStatus::Live);
            Ok(())
        })?;
        let edges_cancel = cancel.child_token();
        let mut edges = JoinSet::new();
        let hosting = Hosting { shared, generation };
        let result = hosting
            .accept(host.as_mut(), commands, &mut edges, &edges_cancel)
            .await;
        edges_cancel.cancel();
        while edges.join_next().await.is_some() {}
        let closed = host.close(CloseMode::Suspend).await;
        if cancel.is_cancelled() || !shared.current(generation) {
            return closed;
        }
        if let Err(error) = result.and(closed) {
            shared.failure(generation, error);
        }
        host_backoff(shared, attempt, cancel).await?;
    }
    Ok(())
}

struct Hosting<'a> {
    shared: &'a Arc<Shared>,
    generation: u32,
}

impl Hosting<'_> {
    async fn accept(
        &self,
        host: &mut dyn crate::transport::HostTransport,
        commands: &mut mpsc::Receiver<HostRequest>,
        edges: &mut JoinSet<Result<()>>,
        cancel: &CancellationToken,
    ) -> Result<()> {
        let Self { shared, generation } = *self;
        let now = shared.clock.now_ms()?;
        let renew_in = host
            .expires_at()
            .saturating_sub(now.saturating_add(60_000))
            .max(1000);
        let renewal = tokio::time::sleep(Duration::from_millis(renew_in));
        tokio::pin!(renewal);
        loop {
            tokio::select! {
                biased;
                () = cancel.cancelled() => return Err(Error::Cancelled),
                () = &mut renewal => return Ok(()),
                command = commands.recv() => match command {
                    Some(HostRequest::Descriptor(reply)) => { let _sent = reply.send(host.descriptor(cancel).await); }
                    None => return Err(Error::Cancelled),
                },
                incoming = host.accept(cancel) => {
                    let stream = incoming?;
                    if edges.len() >= 8 { drop(stream); continue; }
                    let bridge = Bridge { shared: shared.clone(), generation, stream, invitation: None, cancel: cancel.child_token() };
                    let _task = edges.spawn(bridge.run());
                },
                completed = edges.join_next(), if !edges.is_empty() => {
                    if let Some(Ok(Err(error))) = completed && error != Error::Cancelled { shared.failure(generation, error); }
                },
            }
        }
    }
}

async fn host_backoff(shared: &Shared, attempt: u32, cancel: &CancellationToken) -> Result<()> {
    let delay = shared.access(|state| {
        let _updated = state.host.update(attempt, ConnectionStatus::Waiting);
        Ok(state.host.retry_delay_ms())
    })?;
    sleep(cancel, delay).await
}

pub(super) async fn peer_loop(
    shared: Arc<Shared>,
    generation: u32,
    fingerprint: String,
    cancel: CancellationToken,
) -> Result<()> {
    let result = connecting(&shared, generation, &fingerprint, &cancel).await;
    if let Err(error) = result
        && error != Error::Cancelled
    {
        shared.failure(generation, error);
    }
    result
}

async fn connecting(
    shared: &Arc<Shared>,
    generation: u32,
    fingerprint: &str,
    cancel: &CancellationToken,
) -> Result<()> {
    while shared.current(generation) && !cancel.is_cancelled() {
        if shared.access(|state| {
            Ok(state
                .edges
                .values()
                .any(|edge| edge.fingerprint.as_deref() == Some(fingerprint)))
        })? {
            sleep(cancel, 1000).await?;
            continue;
        }
        let attempt = shared.access(|state| {
            state
                .peers
                .get_mut(fingerprint)
                .ok_or(Error::Forbidden)?
                .begin()
                .ok_or(Error::Invalid)
        })?;
        let invitation = approved_invitation(shared, generation, fingerprint, cancel).await;
        let invitation = match invitation {
            Ok(invitation) => invitation,
            Err(error)
                if matches!(
                    error,
                    Error::Expired | Error::Forbidden | Error::Version | Error::Invalid
                ) =>
            {
                shared.access(|state| {
                    if let Some(peer) = state.peers.get_mut(fingerprint) {
                        let status = if error == Error::Expired {
                            ConnectionStatus::Expired
                        } else {
                            ConnectionStatus::Failed
                        };
                        let _updated = peer.update(attempt, status);
                    }
                    Ok(())
                })?;
                shared.failure(generation, error);
                return Ok(());
            }
            Err(error) => {
                shared.failure(generation, error);
                peer_backoff(shared, fingerprint, attempt, cancel).await?;
                continue;
            }
        };
        let connection = bounded(
            cancel,
            Duration::from_mins(1),
            shared.relay.connect(&invitation, cancel),
        )
        .await;
        let result = match connection {
            Ok(mut client) => {
                let result = if shared.invitation(generation, fingerprint).as_ref()
                    != Ok(&invitation)
                    || cancel.is_cancelled()
                {
                    Err(Error::Cancelled)
                } else {
                    match client.take_stream() {
                        Ok(stream) => {
                            Bridge {
                                shared: shared.clone(),
                                generation,
                                stream,
                                invitation: Some(invitation),
                                cancel: cancel.child_token(),
                            }
                            .run()
                            .await
                        }
                        Err(error) => Err(error),
                    }
                };
                let closed = client.close().await;
                closed?;
                result
            }
            Err(error) => Err(error),
        };
        if cancel.is_cancelled() || !shared.current(generation) {
            return Ok(());
        }
        if let Err(error) = result {
            shared.failure(generation, error);
            if matches!(error, Error::Version | Error::Forbidden | Error::Invalid) {
                shared.access(|state| {
                    if let Some(peer) = state.peers.get_mut(fingerprint) {
                        let _updated = peer.update(attempt, ConnectionStatus::Failed);
                    }
                    Ok(())
                })?;
                return Ok(());
            }
        }
        peer_backoff(shared, fingerprint, attempt, cancel).await?;
    }
    Ok(())
}

async fn approved_invitation(
    shared: &Shared,
    generation: u32,
    fingerprint: &str,
    cancel: &CancellationToken,
) -> Result<Invitation> {
    let mut invitation = shared.invitation(generation, fingerprint)?;
    let approved = shared.engine.devices(&invitation.space).await?;
    if !approved.contains(&invitation.host) {
        return Err(Error::Forbidden);
    }
    if !shared.current(generation) || cancel.is_cancelled() {
        return Err(Error::Cancelled);
    }
    let now = shared.clock.now_ms()?;
    if token_expiration(&invitation.connect_token)? <= now.saturating_add(60_000)
        && let Some(replacement) = bounded(
            cancel,
            Duration::from_secs(30),
            shared.credentials.renew(&invitation, cancel),
        )
        .await?
    {
        invitation.validate_renewal(&replacement, &shared.identity, now)?;
        shared.replace_invitation(generation, &invitation, replacement.clone())?;
        invitation = replacement;
    }
    invitation.validate(&shared.identity, now, true)?;
    let resolved = bounded(
        cancel,
        Duration::from_secs(30),
        shared.relay.resolve(&invitation, cancel),
    )
    .await?;
    invitation.validate_renewal(&resolved, &shared.identity, now)?;
    if resolved != invitation {
        shared.replace_invitation(generation, &invitation, resolved.clone())?;
        invitation = resolved;
    }
    Ok(invitation)
}

async fn peer_backoff(
    shared: &Shared,
    fingerprint: &str,
    attempt: u32,
    cancel: &CancellationToken,
) -> Result<()> {
    let delay = shared.access(|state| {
        let peer = state.peers.get_mut(fingerprint).ok_or(Error::Forbidden)?;
        let _updated = peer.update(attempt, ConnectionStatus::Waiting);
        Ok(peer.retry_delay_ms())
    })?;
    sleep(cancel, delay).await
}

async fn sleep(cancel: &CancellationToken, milliseconds: u32) -> Result<()> {
    let jitter = u64::from(
        uuid::Uuid::new_v4()
            .as_bytes()
            .first()
            .copied()
            .unwrap_or_default(),
    );
    tokio::select! {
        biased;
        () = cancel.cancelled() => Err(Error::Cancelled),
        () = tokio::time::sleep(Duration::from_millis(u64::from(milliseconds).saturating_add(jitter))) => Ok(()),
    }
}
