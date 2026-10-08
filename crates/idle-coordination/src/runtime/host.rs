use std::{collections::BTreeMap, io, path::PathBuf, sync::Arc, time::Duration};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::{
    io::{AsyncRead, AsyncWrite},
    sync::mpsc,
    task::JoinSet,
};
use tokio_util::sync::CancellationToken;

use crate::{
    Error, Result,
    clock::{Clock as _, SystemClock},
    dev_tunnels::DevTunnels,
    invitation::HostLease,
    persistence::{FilePersistence, Persistence},
    transport::{BoxStream, CloseMode, HostTransport, RelayDescriptor, RelayProvider},
};

use super::{credentials::GitHubCli, framing};

#[derive(Debug, Deserialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
enum Command {
    Start {
        version: u32,
        state_directory: PathBuf,
        credential_program: PathBuf,
    },
    Send {
        connection_id: u64,
        message: Value,
    },
    Close {
        connection_id: u64,
    },
    Stop {
        remove: bool,
    },
}

#[derive(Debug, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
enum Event {
    Hello { version: u32 },
    Ready { descriptor: RelayDescriptor },
    Unavailable { code: &'static str },
    Opened { connection_id: u64 },
    Message { connection_id: u64, message: Value },
    Closed { connection_id: u64 },
}

enum Outcome {
    Renew,
    Stop { remove: bool },
}

/// Serve the daemon-owned relay helper over a private framed pipe.
/// EOF suspends the relay; an explicit stop can delete the owned cloud resource.
/// # Errors
/// Returns incompatible framing, invalid private paths or incomplete cleanup.
pub async fn serve_relay(
    mut input: impl AsyncRead + Unpin + Send,
    mut output: impl AsyncWrite + Unpin + Send,
    cancel: &CancellationToken,
) -> io::Result<()> {
    framing::write(&mut output, &Event::Hello { version: 1 }).await?;
    let start = tokio::select! {
        () = cancel.cancelled() => return Ok(()),
        start = tokio::time::timeout(Duration::from_secs(10), framing::read(&mut input)) =>
            start.map_err(|_error| framing::invalid())??,
    };
    let Some(Command::Start {
        version: 1,
        state_directory,
        credential_program,
    }) = start
    else {
        return Err(framing::invalid());
    };
    if !state_directory.is_absolute() || !credential_program.is_absolute() {
        return Err(framing::invalid());
    }
    let storage: Arc<dyn Persistence> =
        Arc::new(FilePersistence::open(state_directory).map_err(safe_error)?);
    let relay = DevTunnels::runtime(
        Arc::new(GitHubCli(credential_program)),
        storage.clone(),
        Arc::new(SystemClock),
    )
    .map_err(safe_error)?;
    let lifetime = cancel.child_token();
    let (commands, mut incoming) = mpsc::channel(16);
    let (events, mut outgoing) = mpsc::channel(16);
    let reading = async {
        let result: io::Result<()> = async {
            loop {
                let command = tokio::select! {
                    () = lifetime.cancelled() => return Ok(()),
                    command = framing::read::<Command>(&mut input) => command?,
                };
                let Some(command) = command else {
                    return Ok(());
                };
                if commands.send(command).await.is_err() {
                    return Ok(());
                }
            }
        }
        .await;
        lifetime.cancel();
        result
    };
    let writing = async {
        while let Some(event) = outgoing.recv().await {
            if let Err(error) = framing::write(&mut output, &event).await {
                lifetime.cancel();
                return Err(error);
            }
        }
        Ok(())
    };
    let hosting = async {
        let result = host_loop(&relay, storage.as_ref(), &mut incoming, &events, &lifetime).await;
        lifetime.cancel();
        drop(events);
        result.map_err(safe_error)
    };
    let (read, written, hosted) = tokio::join!(reading, writing, hosting);
    read?;
    written?;
    hosted
}

async fn host_loop(
    relay: &dyn RelayProvider,
    storage: &dyn Persistence,
    commands: &mut mpsc::Receiver<Command>,
    events: &mpsc::Sender<Event>,
    cancel: &CancellationToken,
) -> Result<()> {
    let mut saved = storage.load("runtime-lease")?;
    let mut lease: Option<HostLease> = saved
        .as_deref()
        .map(serde_json::from_slice)
        .transpose()
        .map_err(|_error| Error::Invalid)?;
    relay.cleanup(lease.as_ref(), cancel).await?;
    let mut next_id = 0_u64;
    let mut retry = Duration::from_millis(500);
    loop {
        if cancel.is_cancelled() {
            return Ok(());
        }
        let attempt = relay.host(lease.as_ref(), cancel).await;
        match attempt {
            Ok(mut host) => {
                lease = Some(host.lease());
                let bytes = serde_json::to_vec(&lease).map_err(|_error| Error::Invalid)?;
                if let Err(error) =
                    storage.compare_exchange("runtime-lease", saved.as_deref(), Some(&bytes))
                {
                    let _closed = host.close(CloseMode::Suspend).await;
                    return Err(error);
                }
                saved = Some(bytes);
                retry = Duration::from_millis(500);
                let result =
                    host_attempt(host.as_mut(), commands, events, cancel, &mut next_id).await;
                let remove = matches!(result, Ok(Outcome::Stop { remove: true }));
                host.close(if remove {
                    CloseMode::Remove
                } else {
                    CloseMode::Suspend
                })
                .await?;
                if remove {
                    storage.compare_exchange("runtime-lease", saved.as_deref(), None)?;
                    return Ok(());
                }
                if matches!(result, Ok(Outcome::Stop { remove: false })) {
                    return Ok(());
                }
                if matches!(result, Err(Error::Cancelled)) || cancel.is_cancelled() {
                    return Ok(());
                }
            }
            Err(Error::Cancelled) => return Ok(()),
            Err(_) => {}
        }
        events
            .send(Event::Unavailable {
                code: "relay_unavailable",
            })
            .await
            .map_err(|_error| Error::Cancelled)?;
        tokio::select! {
            () = cancel.cancelled() => return Ok(()),
            command = commands.recv() => match command {
                Some(Command::Stop { remove }) => {
                    if remove && let Some(lease) = &lease {
                        relay.remove(lease, cancel).await?;
                        storage.compare_exchange("runtime-lease", saved.as_deref(), None)?;
                    }
                    return Ok(());
                }
                None => return Ok(()),
                Some(Command::Start { .. } | Command::Send { .. } | Command::Close { .. }) => return Err(Error::Invalid),
            },
            () = tokio::time::sleep(retry) => {},
        }
        retry = retry.saturating_mul(2).min(Duration::from_secs(10));
    }
}

async fn host_attempt(
    host: &mut dyn HostTransport,
    commands: &mut mpsc::Receiver<Command>,
    events: &mpsc::Sender<Event>,
    cancel: &CancellationToken,
    next_id: &mut u64,
) -> Result<Outcome> {
    let descriptor = host.descriptor(cancel).await?;
    let renew_at = host
        .expires_at()
        .min(descriptor.expires_at)
        .saturating_sub(60_000);
    let duration = Duration::from_millis(renew_at.saturating_sub(SystemClock.now_ms()?))
        .min(Duration::from_mins(10));
    if duration.is_zero() {
        return Err(Error::Expired);
    }
    events
        .send(Event::Ready { descriptor })
        .await
        .map_err(|_error| Error::Cancelled)?;
    let lifetime = cancel.child_token();
    let mut clients = BTreeMap::new();
    let mut workers = JoinSet::new();
    let deadline = tokio::time::sleep(duration);
    tokio::pin!(deadline);
    let result = loop {
        tokio::select! {
            biased;
            () = cancel.cancelled() => break Err(Error::Cancelled),
            () = &mut deadline => break Ok(Outcome::Renew),
            command = commands.recv() => match command {
                Some(Command::Send { connection_id, message }) => {
                    if let Some(sender) = clients.get(&connection_id)
                        && mpsc::Sender::try_send(sender, message).is_err() {
                        let _removed = clients.remove(&connection_id);
                    }
                }
                Some(Command::Close { connection_id }) => { let _removed = clients.remove(&connection_id); }
                Some(Command::Stop { remove }) => break Ok(Outcome::Stop { remove }),
                Some(Command::Start { .. }) => break Err(Error::Invalid),
                None => break Err(Error::Cancelled),
            },
            completed = workers.join_next(), if !workers.is_empty() => {
                if let Some(Ok(id)) = completed {
                    let _removed = clients.remove(&id);
                    if events.send(Event::Closed { connection_id: id }).await.is_err() { break Err(Error::Cancelled); }
                }
            }
            stream = host.accept(&lifetime), if clients.len() < 8 => {
                let stream = match stream { Ok(stream) => stream, Err(error) => break Err(error) };
                *next_id = next_id.checked_add(1).ok_or(Error::Invalid)?;
                let id = *next_id;
                let (sender, receiver) = mpsc::channel(8);
                let _previous = clients.insert(id, sender);
                events.send(Event::Opened { connection_id: id }).await.map_err(|_error| Error::Cancelled)?;
                let output = events.clone();
                let stop = lifetime.child_token();
                let _worker = workers.spawn(async move {
                    let _result = forward(stream, id, receiver, output, &stop).await;
                    id
                });
            }
        }
    };
    lifetime.cancel();
    clients.clear();
    while let Some(completed) = workers.join_next().await {
        if let Ok(id) = completed {
            let _sent = events.send(Event::Closed { connection_id: id }).await;
        }
    }
    result
}

async fn forward(
    stream: BoxStream,
    id: u64,
    mut messages: mpsc::Receiver<Value>,
    events: mpsc::Sender<Event>,
    cancel: &CancellationToken,
) -> io::Result<()> {
    let (mut input, mut output) = tokio::io::split(stream);
    let reading = async {
        while let Some(message) = framing::read(&mut input).await? {
            events
                .send(Event::Message {
                    connection_id: id,
                    message,
                })
                .await
                .map_err(|_error| framing::invalid())?;
        }
        Ok(())
    };
    let writing = async {
        while let Some(message) = messages.recv().await {
            framing::write(&mut output, &message).await?;
        }
        Ok(())
    };
    tokio::select! {
        () = cancel.cancelled() => Ok(()),
        result = reading => result,
        result = writing => result,
    }
}

fn safe_error(_error: Error) -> io::Error {
    io::Error::other("Idle runtime relay unavailable")
}
