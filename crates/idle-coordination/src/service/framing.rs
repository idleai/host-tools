use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;
use tokio::{
    io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _},
    sync::mpsc,
};
use tokio_util::sync::CancellationToken;

use crate::{Error, Result, transport::bounded};

use super::{Command, SERVICE_VERSION, Service};

const MAX_FRAME: usize = 16 * 1024 * 1024;
type Controls = Arc<Mutex<BTreeMap<String, CancellationToken>>>;

/// Versioned local service request. IDs correlate responses; mutation request
/// keys inside `command` provide durable retry identity independently of this ID.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ServiceRequest {
    /// Framing/API version, currently one.
    pub version: u16,
    /// Unique ID among this connection's outstanding requests, at most 256 bytes.
    pub id: String,
    /// Cooperative operation timeout, from one to 60,000 milliseconds.
    pub timeout_ms: u32,
    /// Requested operation.
    pub command: Command,
}

/// Framed input. Cancellation is a sideband notification with no separate reply;
/// the original call receives its result after cleanup has finished.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum Message {
    /// Enqueue a bounded service call.
    Call(ServiceRequest),
    /// Cancel a currently queued or running request by its connection-local ID.
    Cancel(String),
}

/// Exactly one response to a valid call; errors contain fixed codes, no tokens.
#[derive(Clone, Deserialize, Serialize)]
pub struct ServiceResponse {
    /// Framing/API version, currently one.
    pub version: u16,
    /// Original connection-local request ID.
    pub id: String,
    /// Typed JSON result (`Ok`) or a fixed error code (`Err`).
    pub result: Result<Value>,
}

impl std::fmt::Debug for ServiceResponse {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ServiceResponse")
            .field("version", &self.version)
            .field("id", &self.id)
            .field("error", &self.result.as_ref().err())
            .finish_non_exhaustive()
    }
}

/// Read one big-endian `u32` byte length and its UTF-8 JSON payload (at most 16 MiB).
/// EOF before a new frame returns `None`; truncated frames are errors.
///
/// # Errors
/// Rejects zero/oversized lengths, malformed JSON and interrupted input.
pub async fn read_frame<T: DeserializeOwned>(
    reader: &mut (impl AsyncRead + Unpin),
) -> Result<Option<T>> {
    let first = match reader.read_u8().await {
        Ok(first) => first,
        Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let mut rest = [0_u8; 3];
    let _read = reader.read_exact(&mut rest).await?;
    let [a, b, c] = rest;
    let length =
        usize::try_from(u32::from_be_bytes([first, a, b, c])).map_err(|_error| Error::Invalid)?;
    if length == 0 || length > MAX_FRAME {
        return Err(Error::Invalid);
    }
    let mut bytes = vec![0; length];
    let _read = reader.read_exact(&mut bytes).await?;
    Ok(Some(serde_json::from_slice(&bytes)?))
}

/// Serialize one bounded JSON response/request and flush it to the local stream.
///
/// # Errors
/// Rejects oversized payloads and reports broken output connections.
pub async fn write_frame<T: Serialize>(
    writer: &mut (impl AsyncWrite + Unpin),
    value: &T,
) -> Result<()> {
    let bytes = serde_json::to_vec(value)?;
    if bytes.is_empty() || bytes.len() > MAX_FRAME {
        return Err(Error::Invalid);
    }
    writer
        .write_u32(u32::try_from(bytes.len()).map_err(|_error| Error::Invalid)?)
        .await?;
    writer.write_all(&bytes).await?;
    writer.flush().await?;
    Ok(())
}

/// Serve a host-authenticated local connection, with at most eight outstanding
/// calls. EOF, cancellation, and framing errors drain peer/native workers before
/// returning. Discovery refresh runs every minute only when explicitly configured.
///
/// # Errors
/// Reports malformed framing, broken output, or incomplete teardown.
pub async fn serve<R, W>(
    service: &mut Service,
    reader: R,
    mut writer: W,
    cancel: &CancellationToken,
) -> Result<()>
where
    R: AsyncRead + Send + Unpin + 'static,
    W: AsyncWrite + Unpin,
{
    let controls = Arc::new(Mutex::new(BTreeMap::new()));
    let (sender, mut receiver) = mpsc::channel(8);
    let reading = tokio::spawn(read_requests(
        reader,
        sender,
        controls.clone(),
        cancel.child_token(),
    ));
    let mut refresh = tokio::time::interval(Duration::from_mins(1));
    refresh.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let result = async {
        loop {
            tokio::select! {
                biased;
                () = cancel.cancelled() => break,
                request = receiver.recv() => {
                    let Some((request, lifetime)) = request else { break; };
                    let ServiceRequest { version, id, timeout_ms, command } = request;
                    let expiry = lifetime.clone();
                    let timer = tokio::spawn(async move {
                        tokio::time::sleep(Duration::from_millis(u64::from(timeout_ms))).await;
                        expiry.cancel();
                    });
                    let outcome = if version == SERVICE_VERSION { service.call(command, &lifetime).await } else { Err(Error::Version) };
                    timer.abort();
                    let response = ServiceResponse { version: SERVICE_VERSION, id: id.clone(), result: outcome };
                    tokio::time::timeout(Duration::from_secs(10), write_frame(&mut writer, &response)).await.map_err(|_error| Error::Timeout)??;
                    let _removed = controls.lock().map_err(|_error| Error::Storage)?.remove(&id);
                }
                _tick = refresh.tick(), if service.directory.is_some() => {
                    let _refreshed = service.refresh_directory(cancel).await;
                }
            }
        }
        Ok(())
    }.await;
    reading.abort();
    let reader_result = match reading.await {
        Ok(result) => result,
        Err(error) if error.is_cancelled() => Ok(()),
        Err(_error) => Err(Error::Transport),
    };
    let cleanup = service.suspend().await;
    result.and(reader_result).and(cleanup)
}

async fn read_requests<R: AsyncRead + Unpin>(
    mut reader: R,
    sender: mpsc::Sender<(ServiceRequest, CancellationToken)>,
    controls: Controls,
    cancel: CancellationToken,
) -> Result<()> {
    let result = async {
        loop {
            let message = tokio::select! {
                biased;
                () = cancel.cancelled() => return Ok(()),
                message = read_frame::<Message>(&mut reader) => message?,
            };
            match message {
                None => return Ok(()),
                Some(Message::Cancel(id)) => {
                    if let Some(lifetime) =
                        controls.lock().map_err(|_error| Error::Storage)?.get(&id)
                    {
                        lifetime.cancel();
                    }
                }
                Some(Message::Call(request)) => {
                    if request.id.is_empty()
                        || request.id.len() > 256
                        || !(1..=60_000).contains(&request.timeout_ms)
                    {
                        return Err(Error::Invalid);
                    }
                    let lifetime = cancel.child_token();
                    {
                        let mut pending = controls.lock().map_err(|_error| Error::Storage)?;
                        if pending.len() >= 8 || pending.contains_key(&request.id) {
                            return Err(Error::Busy);
                        }
                        let _previous = pending.insert(request.id.clone(), lifetime.clone());
                    }
                    sender
                        .try_send((request, lifetime))
                        .map_err(|_error| Error::Busy)?;
                }
            }
        }
    }
    .await;
    for lifetime in controls.lock().map_err(|_error| Error::Storage)?.values() {
        lifetime.cancel();
    }
    result
}

/// Sequential native client usable with pipes or a host-authenticated local socket.
/// An I/O failure closes this logical client; reconcile mutations after reconnecting.
#[derive(Debug)]
pub struct Client<R, W> {
    reader: R,
    writer: W,
    sequence: u64,
    healthy: bool,
}

impl<R: AsyncRead + Unpin, W: AsyncWrite + Unpin> Client<R, W> {
    /// Bind an already authenticated local connection's two halves.
    pub fn new(reader: R, writer: W) -> Self {
        Self {
            reader,
            writer,
            sequence: 0,
            healthy: true,
        }
    }

    /// Execute one request; cancellation sends a sideband notice and waits for
    /// its original response so framing cannot become misaligned.
    ///
    /// # Errors
    /// Returns typed remote failures, malformed responses or an uncertain I/O result.
    pub async fn call(
        &mut self,
        command: Command,
        timeout_ms: u32,
        cancel: &CancellationToken,
    ) -> Result<Value> {
        if !self.healthy {
            return Err(Error::Transport);
        }
        if !(1..=60_000).contains(&timeout_ms) {
            return Err(Error::Invalid);
        }
        if cancel.is_cancelled() {
            return Err(Error::Cancelled);
        }
        self.sequence = self.sequence.checked_add(1).ok_or(Error::Invalid)?;
        let id = self.sequence.to_string();
        self.healthy = false;
        bounded(
            cancel,
            Duration::from_millis(u64::from(timeout_ms)),
            write_frame(
                &mut self.writer,
                &Message::Call(ServiceRequest {
                    version: SERVICE_VERSION,
                    id: id.clone(),
                    timeout_ms,
                    command,
                }),
            ),
        )
        .await?;
        let read = read_frame::<ServiceResponse>(&mut self.reader);
        tokio::pin!(read);
        let response = tokio::select! {
            biased;
            () = cancel.cancelled() => {
                tokio::time::timeout(Duration::from_secs(15), async {
                    write_frame(&mut self.writer, &Message::Cancel(id.clone())).await?;
                    read.await
                }).await.map_err(|_error| Error::Timeout)??
            }
            response = tokio::time::timeout(Duration::from_millis(u64::from(timeout_ms).saturating_add(15_000)), &mut read) => response.map_err(|_error| Error::Timeout)??,
        }.ok_or(Error::Transport)?;
        if response.id != id || response.version != SERVICE_VERSION {
            return Err(Error::Version);
        }
        self.healthy = true;
        response.result
    }
}
