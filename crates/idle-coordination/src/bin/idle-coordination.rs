//! Standalone local coordination service and native peer-v5 worker entrypoint.

use std::{
    io::{self, Read as _},
    time::Duration,
};

use idle_coordination::{
    Error, Result,
    service::{native::Configuration, serve_configuration},
};
use tokio_util::sync::CancellationToken;
use {
    async_trait as _, base64 as _, blake3 as _, idle_history as _, idle_protocol as _,
    reqwest as _, russh as _, serde as _, sha2 as _, tempfile as _, thiserror as _, tunnels as _,
    url as _, uuid as _,
};
#[cfg(test)]
use {editchain_core as _, editchain_store as _};

fn main() -> Result<()> {
    let mut arguments = std::env::args().skip(1);
    let mode = arguments.next().ok_or(Error::Invalid)?;
    if mode == "--peer-worker" {
        if arguments.next().is_some() {
            return Err(Error::Invalid);
        }
        return editchain_sync::run_worker(&mut io::stdin().lock(), &mut io::stdout().lock())
            .map_err(Error::from);
    }
    if mode != "--config" {
        return Err(Error::Invalid);
    }
    let path = arguments.next().ok_or(Error::Invalid)?;
    if arguments.next().is_some() {
        return Err(Error::Invalid);
    }
    let mut bytes = Vec::new();
    let _read = std::fs::File::open(path)?
        .take(65_537)
        .read_to_end(&mut bytes)?;
    if bytes.len() > 65_536 {
        return Err(Error::Invalid);
    }
    let configuration: Configuration = serde_json::from_slice(&bytes)?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    let result = runtime.block_on(run(configuration));
    // Tokio's stdin reader may still be blocked in the OS after a termination signal.
    // All service-owned transports and native workers have already been drained.
    runtime.shutdown_timeout(Duration::from_secs(1));
    result
}

async fn run(configuration: Configuration) -> Result<()> {
    let cancel = CancellationToken::new();
    let interrupted = cancel.clone();
    let signals = tokio::spawn(async move {
        shutdown_signal().await;
        interrupted.cancel();
    });
    let result = serve_configuration(
        configuration,
        tokio::io::stdin(),
        tokio::io::stdout(),
        &cancel,
    )
    .await;
    signals.abort();
    result
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        if let Ok(mut terminated) =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        {
            tokio::select! { _signal = tokio::signal::ctrl_c() => {}, _signal = terminated.recv() => {} }
            return;
        }
    }
    let _signal = tokio::signal::ctrl_c().await;
}
