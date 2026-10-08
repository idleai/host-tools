//! Bounded commit patches with no shell, external diff driver, or text conversion.

use std::{
    io::{self, Read},
    path::Path,
    process::{Command, Stdio},
    sync::mpsc,
    time::Duration,
};

const MAXIMUM: u64 = 8 * 1024 * 1024;

pub(super) fn show(root: &Path, oid: &str) -> io::Result<String> {
    let mut command = Command::new("git");
    for (key, _) in std::env::vars_os().filter(|(key, _)| key.to_string_lossy().starts_with("GIT_"))
    {
        let _command = command.env_remove(key);
    }
    let mut child = command
        .args(["--no-pager", "--no-optional-locks", "-C"])
        .arg(root)
        .args([
            "show",
            "--no-ext-diff",
            "--no-textconv",
            "--no-show-signature",
            "--format=fuller",
            "--stat",
            "--patch",
            "--root",
            oid,
            "--",
        ])
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .stdout(Stdio::piped())
        .spawn()?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("Git output is unavailable."))?;
    let (sender, receiver) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let result = stdout
            .take(MAXIMUM.saturating_add(1))
            .read_to_end(&mut bytes)
            .map(|_| bytes);
        let _sent = sender.send(result);
    });
    let result = receiver
        .recv_timeout(Duration::from_secs(5))
        .map_err(|_error| io::Error::other("The commit patch exceeded its read deadline."))
        .and_then(std::convert::identity)
        .and_then(|bytes| {
            if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAXIMUM {
                Err(io::Error::other(
                    "The commit patch exceeds the document size limit.",
                ))
            } else {
                String::from_utf8(bytes)
                    .map_err(|_error| io::Error::other("The commit patch contains non-UTF-8 text."))
            }
        });
    if result.is_err() {
        let _killed = child.kill();
    }
    let status = child.wait()?;
    let _joined = reader.join();
    if !status.success() && result.is_ok() {
        return Err(io::Error::other("The commit patch could not be read."));
    }
    result
}
