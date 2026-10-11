//! Supervised native host. All workspace installations arrive over its private pipe.

use std::{io, time::Duration};
#[cfg(test)]
use tempfile as _;
use tokio_util::sync::CancellationToken;
use {
    idle_coordination as _, idle_editor_capture as _, idle_history_collector as _,
    idle_history_native as _, idle_host_io as _, idle_repository as _, serde as _, serde_json as _,
};

fn main() -> io::Result<()> {
    let arguments: Vec<_> = std::env::args_os().skip(1).collect();
    let relay = arguments.as_slice() == ["--runtime-relay"];
    let authority = arguments.as_slice() == ["--runtime-authority"];
    if !arguments.is_empty() && !relay && !authority {
        return Err(io::Error::other(
            "native host accepts only --runtime-relay or --runtime-authority",
        ));
    }
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    let result = runtime.block_on(async {
        let cancel = CancellationToken::new();
        let interrupted = cancel.clone();
        let signals = tokio::spawn(async move {
            shutdown_signal().await;
            interrupted.cancel();
        });
        let result = if relay {
            idle_coordination::runtime::serve_relay(
                tokio::io::stdin(),
                tokio::io::stdout(),
                &cancel,
            )
            .await
        } else if authority {
            idle_coordination::runtime::serve_authority(
                tokio::io::stdin(),
                tokio::io::stdout(),
                &cancel,
            )
            .await
        } else {
            idle_host::serve(tokio::io::stdin(), tokio::io::stdout(), &cancel).await
        };
        signals.abort();
        result
    });
    runtime.shutdown_timeout(Duration::from_secs(1));
    result
}

async fn shutdown_signal() {
    #[cfg(unix)]
    if let Ok(mut terminated) =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
    {
        tokio::select! { _signal = tokio::signal::ctrl_c() => {}, _signal = terminated.recv() => {} }
        return;
    }
    let _signal = tokio::signal::ctrl_c().await;
}
