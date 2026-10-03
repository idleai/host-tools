//! Folder-bound collection through IPC or a standalone watch loop.

use idle_history_collector::{Binding, Collector, Mode, Poll, Update};
use std::{
    io,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
#[cfg(test)]
use tempfile as _;
use {
    blake3 as _, editchain_core as _, editchain_engine as _, editchain_git as _,
    editchain_store as _, idle_history_import as _, serde as _,
};

fn main() -> io::Result<()> {
    let mut arguments = std::env::args().skip(1);
    let first = arguments
        .next()
        .ok_or_else(|| io::Error::other("collector binding required"))?;
    let watch = first == "--watch";
    let binding = if watch {
        arguments
            .next()
            .ok_or_else(|| io::Error::other("watch binding required"))?
    } else {
        first
    };
    if arguments.next().is_some() {
        return Err(io::Error::other("expected one collector binding"));
    }
    let binding = serde_json::from_str(&binding).map_err(io::Error::other)?;
    if watch {
        run_watch(binding)
    } else {
        idle_history_collector::service::serve(io::stdin().lock(), io::stdout().lock(), binding)
    }
}

fn run_watch(binding: Binding) -> io::Result<()> {
    let stopped = Arc::new(AtomicBool::new(false));
    let requested = Arc::clone(&stopped);
    let thread = std::thread::current();
    ctrlc::set_handler(move || {
        requested.store(true, Ordering::Relaxed);
        thread.unpark();
    })
    .map_err(io::Error::other)?;
    let mut collector = Collector::new(binding)?;
    let mut output = io::stdout().lock();
    while !stopped.load(Ordering::Relaxed) {
        let pending = match collector.scan(Mode::Import) {
            Ok(update) => {
                let pending = update.pending;
                if update.changed || pending {
                    report(&mut output, &Ok(update))?;
                }
                pending
            }
            Err(error) => {
                report(&mut output, &Err(error.to_string()))?;
                match collector.poll(&Poll::default()) {
                    Ok(update) if update.changed => report(&mut output, &Ok(update))?,
                    Ok(_) => {}
                    Err(error) => report(&mut output, &Err(error.to_string()))?,
                }
                false
            }
        };
        if !pending {
            std::thread::park_timeout(Duration::from_secs(1));
        }
    }
    Ok(())
}

fn report(output: &mut impl io::Write, result: &Result<Update, String>) -> io::Result<()> {
    serde_json::to_writer(&mut *output, result).map_err(io::Error::other)?;
    output.write_all(b"\n")?;
    output.flush()
}
