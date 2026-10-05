//! Framed polling requests for one immutable collector installation.

use std::io::{self, Read, Write};

use serde::{Deserialize, Serialize};

use crate::{Binding, Collector, ImportCancellation, Mode, Poll, Update};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    id: u64,
    body: Action,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum Action {
    Scan(Scan),
    Poll(Poll),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Scan {
    scan: Mode,
}

#[derive(Serialize)]
struct Response {
    id: u64,
    body: Result<Update, String>,
}

/// Serve bounded requests until stdin closes; each response follows durable writes.
///
/// # Errors
/// Returns invalid installation, framing or transport errors.
pub fn serve(mut input: impl Read, mut output: impl Write, binding: Binding) -> io::Result<()> {
    let mut service = Service::new(binding, ImportCancellation::default())?;
    while let Some(bytes) = idle_host_io::read_frame(&mut input, 1024 * 1024)? {
        let bytes = service.handle(&bytes)?;
        idle_host_io::write_frame(&mut output, &bytes, usize::MAX)?;
    }
    Ok(())
}

/// Ordered collection for one workspace, with a lifetime supplied by the host.
#[derive(Debug)]
pub struct Service {
    collector: Collector,
}

impl Service {
    /// Prepare collection without starting the exporter until import work arrives.
    /// # Errors
    /// Rejects invalid bindings or unavailable history.
    pub fn new(binding: Binding, cancellation: ImportCancellation) -> io::Result<Self> {
        Ok(Self {
            collector: Collector::with_cancellation(binding, cancellation)?,
        })
    }

    /// Execute one bounded pass, acknowledging only durable changes.
    /// # Errors
    /// Returns malformed requests or serialization failures.
    pub fn handle(&mut self, bytes: &[u8]) -> io::Result<Vec<u8>> {
        if bytes.len() > 1024 * 1024 {
            return Err(io::Error::other("collector request exceeds its limit"));
        }
        let request: Request = serde_json::from_slice(bytes).map_err(io::Error::other)?;
        let response = Response {
            id: request.id,
            body: match request.body {
                Action::Scan(scan) => self.collector.scan(scan.scan),
                Action::Poll(poll) => self.collector.poll(&poll),
            }
            .map_err(|error| error.to_string()),
        };
        serde_json::to_vec(&response).map_err(io::Error::other)
    }
}
