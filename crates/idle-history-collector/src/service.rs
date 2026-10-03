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
    loop {
        let mut header = [0; 4];
        let Some((first, rest)) = header.split_first_mut() else {
            return Err(io::Error::other("empty header"));
        };
        if input.read(std::slice::from_mut(first))? == 0 {
            return Ok(());
        }
        input.read_exact(rest)?;
        let length = usize::try_from(u32::from_le_bytes(header)).map_err(io::Error::other)?;
        if !(1..=1024 * 1024).contains(&length) {
            return Err(io::Error::other("invalid collector request length"));
        }
        let mut bytes = vec![0; length];
        input.read_exact(&mut bytes)?;
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
        output.write_all(
            &u32::try_from(bytes.len())
                .map_err(io::Error::other)?
                .to_le_bytes(),
        )?;
        output.write_all(&bytes)?;
        output.flush()?;
    }
}
