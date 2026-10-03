use std::{sync::Arc, time::Duration};

use tokio::{
    io::{AsyncReadExt as _, AsyncWriteExt as _},
    time::{Instant, MissedTickBehavior},
};
use tokio_util::sync::CancellationToken;

use crate::{
    Error, Result,
    engine::{NativePeer, Turn},
    transport::{BoxStream, bounded},
};

use super::shared::Shared;

pub(super) struct Bridge {
    pub shared: Arc<Shared>,
    pub generation: u32,
    pub stream: BoxStream,
    pub invitation: Option<crate::invitation::Invitation>,
    pub cancel: CancellationToken,
}

impl Bridge {
    pub(super) async fn run(mut self) -> Result<()> {
        let (space, fingerprint, certificate) = if let Some(invitation) = &self.invitation {
            (
                invitation.space.clone(),
                Some(invitation.host.fingerprint.clone()),
                Some(invitation.host.certificate.clone()),
            )
        } else {
            let space = self.shared.access(|state| {
                state
                    .saved
                    .as_ref()
                    .map(|saved| saved.space.clone())
                    .ok_or(Error::Invalid)
            })?;
            (space, None, None)
        };
        let id = self.shared.add_edge(
            self.generation,
            fingerprint,
            self.invitation.is_some(),
            self.cancel.clone(),
        )?;
        let worker = self.shared.engine.open_peer(space, certificate).await;
        let result = match worker {
            Ok(mut worker) => {
                let result = self.transfer(id, &mut worker).await;
                let closed = worker.close().await;
                result.and(closed)
            }
            Err(error) => Err(error),
        };
        self.shared.remove_edge(id);
        result
    }

    async fn transfer(&mut self, id: u64, worker: &mut NativePeer) -> Result<()> {
        let started = Instant::now();
        let mut last_input = started;
        let mut accepted = false;
        let now = self.shared.clock.now_ms()?;
        let renew_at = self
            .invitation
            .as_ref()
            .map(|invitation| crate::invitation::token_expiration(&invitation.connect_token))
            .transpose()?
            .map(|expires| {
                if expires > now.saturating_add(60_000) {
                    expires.saturating_sub(60_000)
                } else {
                    expires
                }
            });
        let mut interval = tokio::time::interval(Duration::from_millis(1500));
        interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
        let mut buffer = vec![0_u8; editchain_sync::MAX_BRIDGE_BYTES];
        let first = worker.turn(Vec::new(), false).await?;
        self.output(id, first).await?;
        loop {
            let (bytes, tick) = tokio::select! {
                biased;
                () = self.cancel.cancelled() => return Err(Error::Cancelled),
                result = self.stream.read(&mut buffer) => {
                    let count = result.map_err(|_error| Error::Transport)?;
                    if count == 0 { return Err(Error::Transport); }
                    last_input = Instant::now();
                    (buffer.get(..count).ok_or(Error::Invalid)?.to_vec(), false)
                }
                _tick = interval.tick() => {
                    if (!accepted && started.elapsed() > Duration::from_secs(30)) || last_input.elapsed() > Duration::from_secs(90) {
                        return Err(Error::Timeout);
                    }
                    if let Some(deadline) = renew_at
                        && self.shared.clock.now_ms()? >= deadline { return Err(Error::Expired); }
                    (Vec::new(), true)
                }
            };
            let turn = worker.turn(bytes, tick).await?;
            accepted = turn.progress.accepted;
            self.output(id, turn).await?;
        }
    }

    async fn output(&mut self, id: u64, turn: Turn) -> Result<()> {
        // Native durability has completed. Publish it before a blocked outgoing ACK.
        self.shared
            .progress(self.generation, id, turn.device.as_ref(), turn.progress)?;
        if !turn.bytes.is_empty() {
            bounded(&self.cancel, Duration::from_secs(30), async {
                self.stream
                    .write_all(&turn.bytes)
                    .await
                    .map_err(|_error| Error::Transport)
            })
            .await?;
        }
        Ok(())
    }
}
