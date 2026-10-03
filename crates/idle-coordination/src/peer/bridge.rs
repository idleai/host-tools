use std::{collections::VecDeque, sync::Arc, time::Duration};

use tokio::{
    io::{AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _},
    time::{Instant, MissedTickBehavior},
};
use tokio_util::sync::CancellationToken;

use crate::{Error, Result, engine::NativePeer, transport::BoxStream};

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
        let mut output = Output::new();
        let (mut reader, mut writer) = tokio::io::split(&mut self.stream);
        let first = worker.turn(Vec::new(), false).await?;
        self.shared
            .progress(self.generation, id, first.device.as_ref(), first.progress)?;
        output.append(first.bytes)?;
        loop {
            let (bytes, tick) = tokio::select! {
                biased;
                () = self.cancel.cancelled() => return Err(Error::Cancelled),
                written = output.write_some(&mut writer), if !output.chunks.is_empty() => {
                    written?;
                    continue;
                }
                _tick = interval.tick() => {
                    if (!accepted && started.elapsed() > Duration::from_secs(30)) || last_input.elapsed() > Duration::from_secs(90) {
                        return Err(Error::Timeout);
                    }
                    if let Some(deadline) = renew_at
                        && self.shared.clock.now_ms()? >= deadline { return Err(Error::Expired); }
                    (Vec::new(), true)
                }
                result = reader.read(&mut buffer) => {
                    let count = result.map_err(|_error| Error::Transport)?;
                    if count == 0 { return Err(Error::Transport); }
                    last_input = Instant::now();
                    (buffer.get(..count).ok_or(Error::Invalid)?.to_vec(), false)
                }
            };
            let turn = worker.turn(bytes, tick).await?;
            accepted = turn.progress.accepted;
            // Publish native durability before the remote reader accepts our ACK.
            self.shared
                .progress(self.generation, id, turn.device.as_ref(), turn.progress)?;
            output.append(turn.bytes)?;
        }
    }
}

struct Output {
    chunks: VecDeque<Vec<u8>>,
    offset: usize,
    queued: usize,
    last_write: Instant,
}

impl Output {
    fn new() -> Self {
        Self {
            chunks: VecDeque::new(),
            offset: 0,
            queued: 0,
            last_write: Instant::now(),
        }
    }

    fn append(&mut self, bytes: Vec<u8>) -> Result<()> {
        if bytes.is_empty() {
            return Ok(());
        }
        // Retain at most two native turns while still processing incoming data.
        // Excess output closes the edge instead of blocking its reader indefinitely.
        let queued = self.queued.saturating_add(bytes.len());
        if queued > editchain_sync::MAX_BRIDGE_OUTPUT.saturating_mul(2) {
            return Err(Error::Busy);
        }
        if self.chunks.is_empty() {
            self.last_write = Instant::now();
        }
        self.queued = queued;
        self.chunks.push_back(bytes);
        Ok(())
    }

    async fn write_some(&mut self, writer: &mut (impl AsyncWrite + Unpin)) -> Result<()> {
        let deadline = self
            .last_write
            .checked_add(Duration::from_secs(30))
            .ok_or(Error::Invalid)?;
        let chunk = self.chunks.front().ok_or(Error::Invalid)?;
        let bytes = chunk.get(self.offset..).ok_or(Error::Invalid)?;
        let written = tokio::time::timeout_at(deadline, writer.write(bytes))
            .await
            .map_err(|_error| Error::Timeout)?
            .map_err(|_error| Error::Transport)?;
        if written == 0 || written > bytes.len() {
            return Err(Error::Transport);
        }
        self.offset = self.offset.checked_add(written).ok_or(Error::Invalid)?;
        if self.offset == chunk.len() {
            self.queued = self.queued.saturating_sub(chunk.len());
            let _sent = self.chunks.pop_front();
            self.offset = 0;
        }
        self.last_write = Instant::now();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{Duration, Instant, Output};
    use crate::Error;

    #[test]
    fn blocked_peer_output_is_bounded_without_discarding_queued_bytes() {
        let mut output = Output::new();
        for _turn in 0..2 {
            output
                .append(vec![1; editchain_sync::MAX_BRIDGE_OUTPUT])
                .expect("bounded native turn");
        }
        assert_eq!(
            output.append(vec![2]),
            Err(Error::Busy),
            "a peer that never reads cannot grow the output queue"
        );
        assert_eq!(
            output.chunks.len(),
            2,
            "rejected output must preserve existing ordered chunks"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn more_input_cannot_extend_a_blocked_peer_write_deadline() {
        let mut output = Output::new();
        output.append(vec![1; 2]).expect("first native turn");
        let (mut writer, _unread_peer) = tokio::io::duplex(1);
        output
            .write_some(&mut writer)
            .await
            .expect("first byte fits");
        tokio::time::advance(Duration::from_secs(29)).await;
        output.append(vec![2]).expect("another native turn");
        let started = Instant::now();
        let result =
            tokio::time::timeout(Duration::from_secs(2), output.write_some(&mut writer)).await;
        assert_eq!(
            result.expect("original stall deadline must still apply"),
            Err(Error::Timeout),
            "blocked writes must expire despite new input"
        );
        assert_eq!(
            started.elapsed(),
            Duration::from_secs(1),
            "adding output cannot reset the stall timer"
        );
    }
}
