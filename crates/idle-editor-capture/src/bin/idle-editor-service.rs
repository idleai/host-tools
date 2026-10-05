//! Standalone capture entrypoint over the shared editor service.

use std::io;
#[cfg(test)]
use tempfile as _;
use {
    blake3 as _, editchain_core as _, editchain_git as _, editchain_store as _, idle_history as _,
    idle_host_io as _, serde as _, serde_json as _,
};

fn main() -> io::Result<()> {
    idle_editor_capture::service::serve(io::stdin().lock(), io::stdout().lock())
}
