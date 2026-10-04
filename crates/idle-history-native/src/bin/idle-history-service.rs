//! Exact native history documents over the extension host's framed transport.

use idle_host_io as _;

use std::io;

use idle_history_native::history::service;
use {
    editchain_core as _, editchain_engine as _, idle_editor_capture as _, idle_history as _,
    idle_protocol as _, serde as _,
};

#[cfg(test)]
use {idle_history_import as _, tempfile as _};

fn main() -> io::Result<()> {
    let mut arguments = std::env::args().skip(1);
    let binding = arguments
        .next()
        .ok_or_else(|| io::Error::other("explicit history binding required"))?;
    if arguments.next().is_some() {
        return Err(io::Error::other("expected exactly one history binding"));
    }
    let binding: service::Binding = serde_json::from_str(&binding).map_err(io::Error::other)?;
    service::serve(io::stdin().lock(), io::stdout().lock(), &binding)
}

use idle_repository as _;
