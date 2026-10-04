//! Read-only checkout inspection with bounded output and cancellable Git children.

mod remote;
#[cfg(test)]
mod tests;

use std::{collections::BTreeMap, io, path::Path, process::Stdio, time::Duration};

use idle_protocol::v1::repository::{
    GitAuthor, GitCheckout, ReadReport, ReadState, WorktreeStatus,
};
use tokio::{io::AsyncReadExt, process::Command, time::timeout};

pub(crate) use remote::GithubRemote;

#[derive(Debug)]
pub(crate) struct Read {
    pub(crate) checkout: Option<GitCheckout>,
    pub(crate) authors: Vec<GitAuthor>,
    pub(crate) github: Option<GithubRemote>,
    pub(crate) reports: Vec<ReadReport>,
}

pub(crate) async fn read(root: &Path, now: u64) -> Read {
    let mut result = Read {
        checkout: None,
        authors: Vec::new(),
        github: None,
        reports: Vec::new(),
    };
    match checkout(root).await {
        Ok(mut checkout) => {
            result.reports.push(crate::report(
                "git.checkout",
                ReadState::Complete,
                "Selected local Git checkout; no fetch performed.",
                now,
            ));
            match status(root).await {
                Ok(status) => checkout.status = Some(status),
                Err(error) => result.reports.push(crate::report(
                    "git.status",
                    ReadState::Unavailable,
                    error.to_string(),
                    now,
                )),
            }
            if checkout.head.is_some() {
                match authors(root).await {
                    Ok((authors, more)) => {
                        result.authors = authors;
                        result.reports.push(crate::report("git.authors", if more { ReadState::Partial } else { ReadState::Complete }, "Authors from up to 500 commits reachable from this checkout's HEAD; commit authorship does not indicate presence.", now));
                    }
                    Err(error) => result.reports.push(crate::report(
                        "git.authors",
                        ReadState::Unavailable,
                        error.to_string(),
                        now,
                    )),
                }
            } else {
                result.reports.push(crate::report(
                    "git.authors",
                    ReadState::Complete,
                    "No commits on this unborn branch.",
                    now,
                ));
            }
            result.github = checkout.remote.as_deref().and_then(GithubRemote::parse);
            result.checkout = Some(checkout);
        }
        Err(error) => result.reports.push(crate::report(
            "git.checkout",
            ReadState::Unavailable,
            error.to_string(),
            now,
        )),
    }
    result
}

async fn checkout(root: &Path) -> io::Result<GitCheckout> {
    if !root.is_dir() {
        return Err(io::Error::other(
            "The selected workspace folder is unavailable.",
        ));
    }
    let top = required(root, &["rev-parse", "--show-toplevel"])
        .await
        .map_err(|_error| {
            io::Error::other("The selected folder is not an accessible Git worktree.")
        })?;
    let git_directory =
        required(root, &["rev-parse", "--path-format=absolute", "--git-dir"]).await?;
    let common_directory = required(
        root,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    )
    .await?;
    let branch = optional(root, &["symbolic-ref", "--quiet", "--short", "HEAD"]).await?;
    let head = optional(root, &["rev-parse", "--verify", "HEAD^{commit}"]).await?;
    let branch_remote = if let Some(branch) = &branch {
        optional(
            root,
            &["config", "--get", &format!("branch.{branch}.remote")],
        )
        .await?
    } else {
        None
    };
    let remote_name = branch_remote
        .filter(|name| name != ".")
        .unwrap_or_else(|| "origin".into());
    // Read the configured URL without expanding credential-bearing insteadOf rules.
    let remote = optional(
        root,
        &["config", "--get", &format!("remote.{remote_name}.url")],
    )
    .await?;
    let name = remote.as_ref().map(|_| remote_name);
    Ok(GitCheckout {
        root: top,
        git_directory,
        common_directory,
        branch,
        head,
        remote_name: name,
        remote: remote.as_deref().and_then(remote::sanitize),
        status: None,
    })
}

async fn status(root: &Path) -> io::Result<WorktreeStatus> {
    let output = run(
        root,
        &[
            "status",
            "--porcelain=v1",
            "-z",
            "--untracked-files=normal",
            "--ignore-submodules=all",
        ],
        1024 * 1024,
    )
    .await?;
    if !output.success {
        return Err(io::Error::other("Git worktree status is unavailable."));
    }
    parse_status(&output.bytes)
}

fn parse_status(bytes: &[u8]) -> io::Result<WorktreeStatus> {
    let mut result = WorktreeStatus {
        staged: 0,
        unstaged: 0,
        untracked: 0,
        conflicted: 0,
    };
    let mut rows = bytes.split(|byte| *byte == 0).filter(|row| !row.is_empty());
    while let Some(row) = rows.next() {
        let (Some(&index), Some(&worktree)) = (row.first(), row.get(1)) else {
            return Err(io::Error::other("Invalid Git worktree status."));
        };
        if row.get(2) != Some(&b' ') {
            return Err(io::Error::other("Invalid Git worktree status."));
        }
        if index == b'?' && worktree == b'?' {
            result.untracked = result.untracked.saturating_add(1);
            continue;
        }
        if index == b'U'
            || worktree == b'U'
            || matches!((index, worktree), (b'A', b'A') | (b'D', b'D'))
        {
            result.conflicted = result.conflicted.saturating_add(1);
        }
        if index != b' ' {
            result.staged = result.staged.saturating_add(1);
        }
        if worktree != b' ' {
            result.unstaged = result.unstaged.saturating_add(1);
        }
        if (matches!(index, b'R' | b'C') || matches!(worktree, b'R' | b'C'))
            && rows.next().is_none()
        {
            return Err(io::Error::other("Incomplete Git rename status."));
        }
    }
    Ok(result)
}

async fn authors(root: &Path) -> io::Result<(Vec<GitAuthor>, bool)> {
    let output = run(
        root,
        &[
            "log",
            "--no-show-signature",
            "--max-count=501",
            "--format=%an%x00%ae",
            "-z",
            "HEAD",
            "--",
        ],
        1024 * 1024,
    )
    .await?;
    if !output.success {
        return Err(io::Error::other("Git authors are unavailable."));
    }
    let fields = output.bytes.split(|byte| *byte == 0).collect::<Vec<_>>();
    let mut authors = BTreeMap::<(String, String), u32>::new();
    let mut count = 0_u32;
    for pair in fields.chunks_exact(2) {
        count = count.saturating_add(1);
        if count > 500 {
            break;
        }
        let (Some(name), Some(email)) = (pair.first(), pair.get(1)) else {
            continue;
        };
        let key = (
            String::from_utf8_lossy(name).into_owned(),
            String::from_utf8_lossy(email).into_owned(),
        );
        let count = authors.entry(key).or_default();
        *count = count.saturating_add(1);
    }
    Ok((
        authors
            .into_iter()
            .map(|((name, email), commits)| GitAuthor {
                name,
                email,
                commits,
            })
            .collect(),
        count > 500,
    ))
}

async fn required(root: &Path, args: &[&str]) -> io::Result<String> {
    optional(root, args)
        .await?
        .ok_or_else(|| io::Error::other("Git checkout details are unavailable."))
}

async fn optional(root: &Path, args: &[&str]) -> io::Result<Option<String>> {
    let output = run(root, args, 32 * 1024).await?;
    if !output.success {
        return Ok(None);
    }
    let text = String::from_utf8(output.bytes)
        .map_err(|_error| io::Error::other("Git returned a non-UTF-8 checkout value."))?;
    let text = text.strip_suffix('\n').unwrap_or(&text);
    if text.contains(['\n', '\r', '\0']) {
        return Err(io::Error::other(
            "Git returned an unsupported multiline checkout value.",
        ));
    }
    Ok((!text.is_empty()).then(|| text.to_owned()))
}

struct Output {
    success: bool,
    bytes: Vec<u8>,
}

async fn run(root: &Path, args: &[&str], maximum: u64) -> io::Result<Output> {
    let mut command = Command::new("git");
    for (key, _) in std::env::vars_os().filter(|(key, _)| key.to_string_lossy().starts_with("GIT_"))
    {
        let _command = command.env_remove(key);
    }
    let _command = command
        .args([
            "--no-pager",
            "--no-optional-locks",
            "-c",
            "core.fsmonitor=false",
            "-c",
            "core.untrackedCache=false",
            "-c",
            "log.showSignature=false",
            "-C",
        ])
        .arg(root)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("LC_ALL", "C")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    timeout(Duration::from_secs(5), async {
        let mut child = command
            .spawn()
            .map_err(|_error| io::Error::other("Git could not be started on this host."))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("Git output is unavailable."))?;
        let mut bytes = Vec::new();
        let _length = stdout
            .take(maximum.saturating_add(1))
            .read_to_end(&mut bytes)
            .await?;
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > maximum {
            return Err(io::Error::other(
                "Git read reached its output limit; results are unavailable.",
            ));
        }
        Ok(Output {
            success: child.wait().await?.success(),
            bytes,
        })
    })
    .await
    .map_err(|_error| io::Error::other("Git read exceeded its five-second limit."))?
}
