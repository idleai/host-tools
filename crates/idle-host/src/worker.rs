//! Independent ordered service workers; filesystem work never occupies the routing loop.

use idle_history_collector::ImportCancellation;
use std::io;
use tokio::{io::AsyncWriteExt as _, sync::mpsc};
use tokio_util::sync::CancellationToken;

use super::{
    binding::Binding,
    protocol::{Frame, Kind},
    queue::{Output, Queued},
};

type Input = mpsc::Receiver<Queued<Vec<u8>>>;

#[derive(Clone, Default)]
pub(crate) struct Lifetime {
    cancel: CancellationToken,
    imports: ImportCancellation,
}

impl Lifetime {
    pub(crate) fn close(&self) {
        self.cancel.cancel();
        self.imports.cancel();
    }
}

pub(crate) async fn run(
    channel: u32,
    binding: Binding,
    input: Input,
    output: Output,
    lifetime: Lifetime,
) -> io::Result<()> {
    match binding {
        binding @ (Binding::Coordination(_) | Binding::Runtime(_)) => {
            Box::pin(piped(channel, binding, input, output, lifetime.cancel)).await
        }
        Binding::Repository(binding) => {
            repository(channel, binding, input, output, lifetime.cancel).await
        }
        binding @ (Binding::Capture(_) | Binding::History(_) | Binding::Collection(_)) => {
            tokio::task::spawn_blocking(move || {
                blocking(channel, binding, input, &output, &lifetime)
            })
            .await
            .map_err(io::Error::other)?
        }
    }
}

enum Blocking {
    Capture(idle_editor_capture::service::Service),
    History(idle_history_native::history::service::Service),
    Collection(Box<idle_history_collector::service::Service>),
}

impl Blocking {
    fn open(binding: Binding, imports: ImportCancellation) -> io::Result<Self> {
        match binding {
            Binding::Capture(binding) => {
                idle_editor_capture::service::Service::new(binding).map(Self::Capture)
            }
            Binding::History(binding) => {
                idle_history_native::history::service::Service::new(binding).map(Self::History)
            }
            Binding::Collection(binding) => {
                idle_history_collector::service::Service::new(binding, imports)
                    .map(Box::new)
                    .map(Self::Collection)
            }
            Binding::Repository(_) | Binding::Coordination(_) | Binding::Runtime(_) => {
                Err(super::protocol::invalid())
            }
        }
    }

    fn handle(&mut self, bytes: &[u8]) -> io::Result<Vec<u8>> {
        match self {
            Self::Capture(service) => service.handle(bytes),
            Self::History(service) => service.handle(bytes),
            Self::Collection(service) => service.handle(bytes),
        }
    }
}

fn blocking(
    channel: u32,
    binding: Binding,
    mut input: Input,
    output: &Output,
    lifetime: &Lifetime,
) -> io::Result<()> {
    let mut service = Blocking::open(binding, lifetime.imports.clone())?;
    while let Some(request) = input.blocking_recv() {
        if lifetime.cancel.is_cancelled() {
            break;
        }
        let payload = service.handle(&request.value)?;
        if !lifetime.cancel.is_cancelled() {
            output.blocking_send(Frame {
                kind: Kind::Data,
                channel,
                payload,
            })?;
        }
    }
    Ok(())
}

async fn repository(
    channel: u32,
    binding: idle_repository::Binding,
    mut input: Input,
    output: Output,
    cancel: CancellationToken,
) -> io::Result<()> {
    let mut service = idle_repository::service::Service::new(binding)?;
    loop {
        let request = tokio::select! {
            biased;
            () = cancel.cancelled() => break,
            request = input.recv() => request,
        };
        let Some(request) = request else {
            break;
        };
        let payload = tokio::select! {
            biased;
            () = cancel.cancelled() => break,
            response = service.handle(&request.value) => response?,
        };
        output
            .send(Frame {
                kind: Kind::Data,
                channel,
                payload,
            })
            .await?;
    }
    Ok(())
}

async fn piped(
    channel: u32,
    binding: Binding,
    mut input: Input,
    output: Output,
    cancel: CancellationToken,
) -> io::Result<()> {
    // The coordinator's established cancellation and credential exchange runs
    // over bounded in-memory streams. It does not create another OS process.
    let (client, server) = tokio::io::duplex(64 * 1024);
    let (mut responses, mut requests) = tokio::io::split(client);
    let (reader, writer) = tokio::io::split(server);
    let feeding = async {
        loop {
            let request = tokio::select! {
                biased;
                () = cancel.cancelled() => break,
                request = input.recv() => request,
            };
            let Some(request) = request else {
                break;
            };
            let length = u32::try_from(request.value.len()).map_err(io::Error::other)?;
            tokio::select! {
                biased;
                () = cancel.cancelled() => break,
                result = async {
                    requests.write_all(&length.to_be_bytes()).await?;
                    requests.write_all(&request.value).await
                } => result?,
            }
        }
        requests.shutdown().await
    };
    let emitting = async {
        loop {
            let response =
                idle_coordination::service::read_frame::<Box<serde_json::value::RawValue>>(
                    &mut responses,
                )
                .await
                .map_err(io::Error::other)?;
            let Some(response) = response else {
                break;
            };
            output
                .send(Frame {
                    kind: Kind::Data,
                    channel,
                    payload: response.get().as_bytes().to_vec(),
                })
                .await?;
        }
        Ok::<_, io::Error>(())
    };
    let serving = async {
        match binding {
            Binding::Coordination(configuration) => {
                idle_coordination::service::serve_configuration(
                    *configuration,
                    reader,
                    writer,
                    &cancel,
                )
                .await
                .map_err(io::Error::other)
            }
            Binding::Runtime(binding) => {
                idle_coordination::runtime::serve_client(binding, reader, writer, &cancel).await
            }
            Binding::Capture(_)
            | Binding::History(_)
            | Binding::Collection(_)
            | Binding::Repository(_) => Err(super::protocol::invalid()),
        }
    };
    let _completed = tokio::try_join!(feeding, emitting, serving)?;
    Ok(())
}
