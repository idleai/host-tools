//! Portable standalone repository reads with host-installed paths and credentials.
//!
//! Git reads never fetch or change the checkout. GitHub requests use one fixed
//! HTTPS API origin, bounded conditional pages and exact retained response bytes.
//! A recorded session never implies that an agent process is running.

mod git;
mod github;
#[cfg(test)]
mod record_tests;
mod records;
pub mod service;
mod sources;

use std::{
    fmt, io,
    path::PathBuf,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use idle_protocol::v1::{
    projections::ProjectionInput,
    repository::{ReadReport, ReadState, RepositoryScope, RepositorySnapshot},
};
use serde::{Deserialize, Serialize};

pub use sources::{checked_inputs, validate_sources};

/// Immutable paths installed by the file-owning host, never supplied by a webview.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    /// Explicit logical workspace/repository/chain scope.
    pub scope: RepositoryScope,
    /// Selected workspace folder, including a linked worktree when applicable.
    pub root: PathBuf,
    /// Explicit history storage directory.
    pub chain_directory: PathBuf,
}

/// Per-read host credential. Debug output deliberately omits the secret token.
#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Credentials {
    /// Account label supplied by the authenticated host session.
    pub account: String,
    /// Access token, used only in the HTTPS authorization header.
    pub token: String,
}

impl fmt::Debug for Credentials {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Credentials")
            .field("account", &self.account)
            .finish_non_exhaustive()
    }
}

/// Snapshot plus four non-activity inputs, each backed by retained source records.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Snapshot {
    /// Typed repository details and recorded sessions.
    pub repository: RepositorySnapshot,
    /// Tasks, errors, triage and needs-input; activity remains an engine read.
    pub projections: Vec<ProjectionInput>,
}

/// One repository reader and account-scoped conditional-response cache.
#[derive(Debug)]
pub struct Reader {
    binding: Binding,
    github: github::Client,
}

impl Reader {
    /// Validate the explicit installation without executing Git or making requests.
    ///
    /// # Errors
    /// Rejects relative paths, missing scope or an unavailable HTTPS client.
    pub fn new(binding: Binding) -> io::Result<Self> {
        if !binding.root.is_absolute()
            || !binding.chain_directory.is_absolute()
            || [
                &binding.scope.workspace_id,
                &binding.scope.repository_id,
                &binding.scope.chain,
            ]
            .iter()
            .any(|value| value.is_empty() || value.len() > 1024)
        {
            return Err(io::Error::other(
                "Repository reads require an explicit absolute binding.",
            ));
        }
        Ok(Self {
            binding,
            github: github::Client::new()?,
        })
    }

    /// Read the local checkout and recorded sessions without waiting for GitHub.
    ///
    /// # Errors
    /// Rejects an invalid host clock.
    pub async fn read_local(&self) -> io::Result<Snapshot> {
        let now = now_ms()?;
        let git = read_git(&self.binding.root, now).await;
        let github = git.github.as_ref().map_or_else(
            || github::Read::absent(now),
            |remote| github::Read::loading(now, remote),
        );
        Ok(self.snapshot(git, github, None, now))
    }

    /// Read one replacement. Failed sources remain explicit while local data works offline.
    /// Credentials never enter a result, cache key string, error message or stored record.
    ///
    /// # Errors
    /// Rejects malformed credentials or an invalid host clock.
    pub async fn read(
        &mut self,
        credentials: Option<&Credentials>,
        refresh_github: bool,
    ) -> io::Result<Snapshot> {
        if credentials.is_some_and(|value| {
            value.account.is_empty()
                || value.account.len() > 256
                || value.token.is_empty()
                || value.token.len() > 8192
                || value.token.contains(['\r', '\n', '\0'])
        }) {
            return Err(io::Error::other("Invalid host GitHub credential."));
        }
        let now = now_ms()?;
        let git = read_git(&self.binding.root, now).await;
        let github = if let Some(remote) = git.github.as_ref() {
            let query = github::Query {
                binding: &self.binding,
                remote,
                head: git
                    .checkout
                    .as_ref()
                    .and_then(|checkout| checkout.head.as_deref()),
                credentials,
                now,
            };
            tokio::time::timeout(
                Duration::from_secs(20),
                self.github.replacement(&query, refresh_github),
            )
            .await
            .unwrap_or_else(|_error| github::Read::timed_out(now, remote))
        } else {
            github::Read::absent(now)
        };
        Ok(self.snapshot(
            git,
            github,
            credentials.map(|value| value.account.clone()),
            now,
        ))
    }

    fn snapshot(
        &self,
        mut git: git::Read,
        github: github::Read,
        account: Option<String>,
        now: u64,
    ) -> Snapshot {
        let (sessions, sessions_report) = records::sessions(&self.binding.chain_directory, now);
        git.reports.extend(github.reports);
        git.reports.push(sessions_report);
        Snapshot {
            repository: RepositorySnapshot {
                scope: self.binding.scope.clone(),
                checked_at_ms: now,
                checkout: git.checkout,
                github: github.repository,
                account,
                git_authors: git.authors,
                contributors: github.contributors,
                collaborators: github.collaborators,
                sessions,
                reports: git.reports,
            },
            projections: github.inputs,
        }
    }
}

async fn read_git(root: &std::path::Path, now: u64) -> git::Read {
    tokio::time::timeout(Duration::from_secs(10), git::read(root, now))
        .await
        .unwrap_or_else(|_error| git::Read {
            checkout: None,
            authors: Vec::new(),
            github: None,
            reports: vec![report(
                "git.checkout",
                ReadState::Unavailable,
                "The selected Git checkout exceeded its ten-second read deadline.",
                now,
            )],
        })
}

fn now_ms() -> io::Result<u64> {
    u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(io::Error::other)?
            .as_millis(),
    )
    .map_err(io::Error::other)
}

fn report(topic: &str, state: ReadState, message: impl Into<String>, now: u64) -> ReadReport {
    ReadReport {
        topic: topic.into(),
        state,
        message: message.into(),
        checked_at_ms: now,
        retry_at_ms: None,
        source_url: None,
    }
}

fn short_text(text: &str, maximum: usize) -> String {
    let mut chars = text.chars();
    let result: String = chars.by_ref().take(maximum).collect();
    if chars.next().is_some() {
        format!("{result}…")
    } else {
        result
    }
}
