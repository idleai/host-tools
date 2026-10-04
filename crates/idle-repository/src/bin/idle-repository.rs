//! Packaged standalone repository reader; credentials arrive only over framed stdin.

use std::io;

#[cfg(test)]
use tempfile as _;
use {
    blake3 as _, editchain_core as _, editchain_engine as _, idle_protocol as _, reqwest as _,
    serde as _, url as _,
};

#[tokio::main]
async fn main() -> io::Result<()> {
    let mut arguments = std::env::args().skip(1);
    let binding = arguments
        .next()
        .ok_or_else(|| io::Error::other("Repository binding required."))?;
    if arguments.next().is_some() {
        return Err(io::Error::other("Expected one repository binding."));
    }
    let binding = serde_json::from_str(&binding).map_err(io::Error::other)?;
    idle_repository::service::serve(tokio::io::stdin(), tokio::io::stdout(), binding).await
}
