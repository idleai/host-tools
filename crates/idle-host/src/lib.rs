//! One native host multiplexes independently scoped workspace services over one private pipe.
//! Service libraries retain their domain rules, storage and ordered request execution.

mod binding;
mod protocol;
mod queue;
mod worker;

use binding::Open;
use protocol::{Frame, Kind, MAX_CHANNELS, MAX_FRAME, MAX_OPEN, MAX_QUEUED_BYTES};
use queue::{Output, Queued};
use std::{collections::BTreeMap, io, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    sync::{Semaphore, mpsc},
    task::JoinSet,
};
use tokio_util::sync::CancellationToken;

struct Channel {
    input: Option<mpsc::Sender<Queued<Vec<u8>>>>,
    maximum: usize,
    lifetime: worker::Lifetime,
}

impl Channel {
    fn close(&mut self) {
        self.lifetime.close();
        self.input = None;
    }
}

impl Drop for Channel {
    fn drop(&mut self) {
        self.close();
    }
}

struct Host {
    channels: BTreeMap<u32, Channel>,
    workers: JoinSet<(u32, io::Result<()>)>,
    budget: Arc<Semaphore>,
    output: Output,
    last_channel: u32,
}

impl Host {
    async fn frame(&mut self, frame: Frame) -> io::Result<bool> {
        match frame.kind {
            Kind::Open if frame.channel > self.last_channel && frame.payload.len() <= MAX_OPEN => {
                self.last_channel = frame.channel;
                self.open(frame.channel, &frame.payload).await?;
            }
            Kind::Data if frame.channel != 0 => {
                if let Some(channel) = self.channels.get_mut(&frame.channel) {
                    let length = frame.payload.len();
                    let accepted = length <= channel.maximum
                        && channel.input.as_ref().is_some_and(|sender| {
                            Queued::new(frame.payload, length, &self.budget)
                                .is_ok_and(|request| sender.try_send(request).is_ok())
                        });
                    if !accepted {
                        channel.close();
                    }
                } else {
                    self.output
                        .send(Frame::closed(frame.channel, "closed"))
                        .await?;
                }
            }
            Kind::Close if frame.channel != 0 && frame.payload.is_empty() => {
                if let Some(channel) = self.channels.get_mut(&frame.channel) {
                    channel.close();
                } else {
                    self.output
                        .send(Frame::closed(frame.channel, "closed"))
                        .await?;
                }
            }
            Kind::Shutdown if frame.channel == 0 && frame.payload.is_empty() => return Ok(false),
            Kind::Hello
            | Kind::Open
            | Kind::Data
            | Kind::Close
            | Kind::Ready
            | Kind::Closed
            | Kind::Shutdown => {
                return Err(protocol::invalid());
            }
        }
        Ok(true)
    }

    async fn open(&mut self, id: u32, payload: &[u8]) -> io::Result<()> {
        if self.channels.len() >= MAX_CHANNELS {
            return self.output.send(Frame::closed(id, "busy")).await;
        }
        let binding = serde_json::from_slice::<Open>(payload)
            .map_err(io::Error::other)
            .and_then(|open| {
                open.validate()?;
                Ok(open.service)
            });
        let Ok(binding) = binding else {
            return self.output.send(Frame::closed(id, "invalid_request")).await;
        };
        let maximum = binding.maximum();
        let lifetime = worker::Lifetime::default();
        let (sender, input) = mpsc::channel(16);
        let previous = self.channels.insert(
            id,
            Channel {
                input: Some(sender),
                maximum,
                lifetime: lifetime.clone(),
            },
        );
        drop(previous);
        self.output.send(Frame::control(Kind::Ready, id)).await?;
        let output = self.output.clone();
        let _worker = self
            .workers
            .spawn(async move { (id, worker::run(id, binding, input, output, lifetime).await) });
        Ok(())
    }

    async fn completed(
        &mut self,
        result: Result<(u32, io::Result<()>), tokio::task::JoinError>,
    ) -> io::Result<()> {
        let (id, outcome) = result.map_err(io::Error::other)?;
        let removed = self.channels.remove(&id);
        drop(removed);
        self.output
            .send(Frame::closed(
                id,
                if outcome.is_ok() {
                    "closed"
                } else {
                    "unavailable"
                },
            ))
            .await
    }

    async fn read(
        &mut self,
        input: &mut (impl AsyncRead + Unpin),
        cancel: &CancellationToken,
    ) -> io::Result<()> {
        loop {
            // Keep a partial frame alive when a worker finishes between pipe reads.
            let reading = idle_host_io::asynchronous::read_frame(input, MAX_FRAME);
            tokio::pin!(reading);
            loop {
                tokio::select! {
                    biased;
                    () = cancel.cancelled() => return Ok(()),
                    completed = self.workers.join_next(), if !self.workers.is_empty() => {
                        if let Some(completed) = completed { self.completed(completed).await?; }
                    }
                    bytes = &mut reading => {
                        let Some(bytes) = bytes? else { return Ok(()); };
                        if !self.frame(Frame::decode(&bytes)?).await? { return Ok(()); }
                        break;
                    }
                }
            }
        }
    }

    async fn close(&mut self) -> io::Result<()> {
        for channel in self.channels.values_mut() {
            channel.close();
        }
        while let Some(completed) = self.workers.join_next().await {
            self.completed(completed).await?;
        }
        Ok(())
    }
}

/// Serve versioned workspace channels until EOF, shutdown or the process lifetime ends.
/// Blocking operations run on separate workers; each channel has ordered requests,
/// a fixed binding, independent cancellation and bounded queues.
/// # Errors
/// Rejects incompatible framing, failed output and incomplete service teardown.
pub async fn serve(
    mut input: impl AsyncRead + Unpin,
    mut writer: impl AsyncWrite + Unpin + Send + 'static,
    cancel: &CancellationToken,
) -> io::Result<()> {
    let lifetime = cancel.child_token();
    let failed = lifetime.clone();
    let (output, mut responses) = Output::new();
    let mut writing = tokio::spawn(async move {
        let result = async {
            while let Some(response) = responses.recv().await {
                idle_host_io::asynchronous::write_frame(
                    &mut writer,
                    &response.value.encode(),
                    MAX_FRAME,
                )
                .await?;
            }
            Ok::<_, io::Error>(())
        }
        .await;
        failed.cancel();
        result
    });
    output.send(Frame { kind: Kind::Hello, channel: 0,
        payload: br#"{"version":1,"services":{"capture":1,"history":1,"collection":1,"repository":1,"coordination":1,"runtime":1},"features":["repository.local","runtime.workspace","coordination.runtime-owner"]}"#.to_vec() }).await?;
    let mut host = Host {
        channels: BTreeMap::new(),
        workers: JoinSet::new(),
        budget: Arc::new(Semaphore::new(MAX_QUEUED_BYTES)),
        output,
        last_channel: 0,
    };
    let result = host.read(&mut input, &lifetime).await;
    let closed = tokio::time::timeout(Duration::from_secs(8), host.close())
        .await
        .map_err(io::Error::other)
        .and_then(std::convert::identity);
    drop(host);
    let written = tokio::time::timeout(Duration::from_secs(2), &mut writing)
        .await
        .map_err(io::Error::other)
        .and_then(|result| result.map_err(io::Error::other))
        .and_then(std::convert::identity);
    writing.abort();
    result.and(closed).and(written)
}

#[cfg(test)]
mod tests;
