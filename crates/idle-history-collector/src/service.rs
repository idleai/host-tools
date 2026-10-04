//! Framed polling requests for one immutable collector installation.

use std::io::{self, Read, Write};

use serde::{Deserialize, Serialize};

use crate::{Binding, Collector, Mode, Poll, Update};

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
    let mut collector = Collector::new(binding)?;
    while let Some(bytes) = idle_host_io::read_frame(&mut input, 1024 * 1024)? {
        let request: Request = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
        let response = Response {
            id: request.id,
            body: match request.body {
                Action::Scan(scan) => collector.scan(scan.scan),
                Action::Poll(poll) => collector.poll(&poll),
            }
            .map_err(|error| error.to_string()),
        };
        let bytes = serde_json::to_vec(&response).map_err(io::Error::other)?;
        idle_host_io::write_frame(&mut output, &bytes, usize::MAX)?;
    }
    Ok(())
}
