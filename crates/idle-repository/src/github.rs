//! Bounded GitHub REST snapshots mapped from declared repository fields.

mod http;
mod mapping;
#[cfg(test)]
mod tests;

use std::{io, time::Duration};

use idle_protocol::v1::{
    projections::{ProjectionInput, ProjectionReference},
    repository::{GithubPerson, GithubRepository, ReadReport, ReadState},
};
use serde_json::Value;

use crate::{Binding, Credentials, git::GithubRemote};
use http::{Failure, Http};

#[derive(Debug)]
pub(crate) struct Client {
    http: Http,
    recent: Option<Recent>,
}

#[derive(Debug)]
struct Recent {
    key: [u8; 32],
    until: u64,
    read: Read,
}

pub(crate) struct Query<'a> {
    pub(crate) binding: &'a Binding,
    pub(crate) remote: &'a GithubRemote,
    pub(crate) head: Option<&'a str>,
    pub(crate) credentials: Option<&'a Credentials>,
    pub(crate) now: u64,
}

#[derive(Clone, Debug)]
pub(crate) struct Read {
    pub(crate) repository: Option<GithubRepository>,
    pub(crate) contributors: Vec<GithubPerson>,
    pub(crate) collaborators: Vec<GithubPerson>,
    pub(crate) inputs: Vec<ProjectionInput>,
    pub(crate) reports: Vec<ReadReport>,
}

#[derive(Debug)]
struct Dataset {
    rows: Vec<(Value, Option<ProjectionReference>)>,
    report: ReadReport,
    unauthorized: bool,
}

impl Client {
    pub(crate) fn new() -> io::Result<Self> {
        Ok(Self {
            http: Http::new()?,
            recent: None,
        })
    }

    pub(crate) async fn replacement(&mut self, query: &Query<'_>, refresh: bool) -> Read {
        let mut hasher = blake3::Hasher::new();
        for value in [
            Some(query.remote.url().as_str()),
            query.head,
            query.credentials.map(|value| value.account.as_str()),
            query.credentials.map(|value| value.token.as_str()),
        ] {
            let _hash = hasher
                .update(value.unwrap_or_default().as_bytes())
                .update(b"\0");
        }
        let key = *hasher.finalize().as_bytes();
        if !refresh
            && let Some(recent) = &self.recent
            && recent.key == key
            && recent.until > query.now
        {
            let mut read = recent.read.clone();
            for input in &mut read.inputs {
                input.freshness.status = idle_protocol::v1::projections::FreshnessStatus::Unknown;
            }
            if let Some(report) = read.reports.first_mut() {
                report.message.push_str(" Reused a GitHub read less than 60 seconds old; Refresh repository checks the service again.");
            }
            return read;
        }
        self.recent = None;
        let read = self.read(query).await;
        self.recent = Some(Recent {
            key,
            until: query.now.saturating_add(60_000),
            read: read.clone(),
        });
        read
    }

    pub(crate) async fn read(&mut self, query: &Query<'_>) -> Read {
        self.http.bind(&query.remote.url(), query.credentials);
        let path = query.remote.api_path();
        let metadata = match self.http.get(&path, query.credentials, query.now).await {
            Ok(page) => serde_json::from_slice::<Value>(&page.bytes)
                .ok()
                .and_then(|value| mapping::repository(&value)),
            Err(error) => return Read::unavailable(query.now, Some(query.remote), &error),
        };
        let Some(repository) = metadata else {
            return Read::unavailable(
                query.now,
                Some(query.remote),
                &Failure::new("GitHub returned invalid repository metadata."),
            );
        };
        let mut reports = vec![crate::report(
            "github.repository",
            ReadState::Complete,
            "Repository metadata from GitHub REST.",
            query.now,
        )];
        let mut contributors = self.dataset(query, Endpoint::Contributors).await;
        let mut collaborators = self.dataset(query, Endpoint::Collaborators).await;
        let issues = self.dataset(query, Endpoint::Issues).await;
        let pulls = self.dataset(query, Endpoint::Pulls).await;
        let checks = self.dataset(query, Endpoint::Checks).await;
        let runs = self.dataset(query, Endpoint::Runs).await;
        if [
            &contributors,
            &collaborators,
            &issues,
            &pulls,
            &checks,
            &runs,
        ]
        .iter()
        .any(|data| data.unauthorized)
        {
            self.http.forget();
            return Read::unavailable(
                query.now,
                Some(query.remote),
                &Failure {
                    message: "GitHub authentication expired during this read. Sign in again."
                        .into(),
                    retry_at_ms: None,
                    unauthorized: true,
                },
            );
        }
        let people = mapping::people(&mut contributors);
        let accessible = mapping::people(&mut collaborators);
        let inputs = mapping::inputs(&issues, &pulls, &checks, &runs, query.now);
        reports.extend([
            contributors.report,
            collaborators.report,
            issues.report,
            pulls.report,
            checks.report,
            runs.report,
        ]);
        for report in &mut reports {
            report.source_url = Some(query.remote.url());
        }
        Read {
            repository: Some(repository),
            contributors: people,
            collaborators: accessible,
            inputs,
            reports,
        }
    }

    async fn dataset(&mut self, query: &Query<'_>, endpoint: Endpoint) -> Dataset {
        let mut result = Dataset {
            rows: Vec::new(),
            report: crate::report(
                endpoint.topic(),
                ReadState::Complete,
                endpoint.scope(),
                query.now,
            ),
            unauthorized: false,
        };
        if endpoint == Endpoint::Collaborators && query.credentials.is_none() {
            result.report.state = ReadState::Unavailable;
            result.report.message = "Sign in to read accessible GitHub collaborators. Git authors and public contributors remain separate.".into();
            return result;
        }
        if matches!(endpoint, Endpoint::Checks | Endpoint::Runs) && query.head.is_none() {
            result.report.state = ReadState::NotApplicable;
            result.report.message =
                "This checkout has no HEAD commit to query for checks or workflow runs.".into();
            return result;
        }
        for page_number in 1..=2 {
            let path = endpoint.path(query, page_number);
            let page = match self.http.get(&path, query.credentials, query.now).await {
                Ok(page) => page,
                Err(error) => {
                    result.fail(error);
                    break;
                }
            };
            let decoded: Value = match serde_json::from_slice(&page.bytes) {
                Ok(value) => value,
                Err(_error) => {
                    result.fail(Failure::new("GitHub returned invalid JSON."));
                    break;
                }
            };
            let Some(rows) = endpoint.rows(&decoded) else {
                result.fail(Failure::new("GitHub returned an unexpected result shape."));
                break;
            };
            let source = if endpoint.recorded() {
                match retain(query, &path, &page.bytes).await {
                    Ok(reference) => Some(reference),
                    Err(_error) => {
                        result.fail(Failure::new("The exact GitHub response could not be retained in this workspace's history. Refresh to retry."));
                        break;
                    }
                }
            } else {
                None
            };
            for row in rows.iter().take(50) {
                result.rows.push((row.clone(), source.clone()));
            }
            if rows.len() > 50 || (page.more && page_number == 2) {
                result.report.state = ReadState::Partial;
                result.report.message = format!(
                    "{} Additional results remain beyond the two-page, 100-row limit.",
                    endpoint.scope()
                );
            }
            if !page.more {
                break;
            }
        }
        result
    }
}

async fn retain(query: &Query<'_>, path: &str, bytes: &[u8]) -> io::Result<ProjectionReference> {
    for attempt in 0..=3_u64 {
        match crate::records::capture(
            &query.binding.chain_directory,
            &query.binding.scope.repository_id,
            &format!("https://api.github.com{path}"),
            bytes,
        ) {
            Err(error) if error.kind() == io::ErrorKind::WouldBlock && attempt < 3 => {
                tokio::time::sleep(Duration::from_millis(
                    25_u64.saturating_mul(attempt.saturating_add(1)),
                ))
                .await;
            }
            result => return result,
        }
    }
    Err(io::Error::other("The bound history is busy."))
}

impl Read {
    pub(crate) fn absent(now: u64) -> Self {
        let failure = Failure::new(
            "This checkout has no supported github.com remote. Local Git and recorded sessions remain available.",
        );
        let mut result = Self::unavailable(now, None, &failure);
        for report in &mut result.reports {
            report.state = ReadState::NotApplicable;
        }
        result
    }

    pub(crate) fn timed_out(now: u64, remote: &GithubRemote) -> Self {
        Self::unavailable(
            now,
            Some(remote),
            &Failure::new("The GitHub snapshot exceeded its 20-second deadline. Refresh to retry."),
        )
    }

    fn unavailable(now: u64, remote: Option<&GithubRemote>, failure: &Failure) -> Self {
        let mut report = crate::report(
            "github.repository",
            ReadState::Unavailable,
            &failure.message,
            now,
        );
        report.retry_at_ms = failure.retry_at_ms;
        report.source_url = remote.map(GithubRemote::url);
        Self {
            repository: None,
            contributors: Vec::new(),
            collaborators: Vec::new(),
            inputs: mapping::unavailable(now, &failure.message),
            reports: vec![report],
        }
    }
}

impl Dataset {
    fn fail(&mut self, failure: Failure) {
        self.report.state = if self.rows.is_empty() {
            ReadState::Unavailable
        } else {
            ReadState::Partial
        };
        self.report.message = failure.message;
        self.report.retry_at_ms = failure.retry_at_ms;
        self.unauthorized = failure.unauthorized;
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Endpoint {
    Contributors,
    Collaborators,
    Issues,
    Pulls,
    Checks,
    Runs,
}

impl Endpoint {
    const fn topic(self) -> &'static str {
        match self {
            Self::Contributors => "github.contributors",
            Self::Collaborators => "github.collaborators",
            Self::Issues => "github.issues",
            Self::Pulls => "github.pulls",
            Self::Checks => "github.checks",
            Self::Runs => "github.runs",
        }
    }

    const fn scope(self) -> &'static str {
        match self {
            Self::Contributors => {
                "GitHub contributors, up to two pages of 50; GitHub may cache contribution counts."
            }
            Self::Collaborators => {
                "Accessible GitHub collaborators, up to two pages of 50; these are not Idle access grants."
            }
            Self::Issues => {
                "Open issues and pull requests, most recently updated first, up to two pages of 50."
            }
            Self::Pulls => {
                "Open pull requests and requested reviewers, most recently updated first, up to two pages of 50."
            }
            Self::Checks => "Latest check runs for this checkout's HEAD, up to two pages of 50.",
            Self::Runs => "Workflow runs for this checkout's HEAD, up to two pages of 50.",
        }
    }

    fn path(self, query: &Query<'_>, page: u32) -> String {
        let base = query.remote.api_path();
        let endpoint = match self {
            Self::Contributors => "contributors?".into(),
            Self::Collaborators => "collaborators?".into(),
            Self::Issues => "issues?state=open&sort=updated&direction=desc&".into(),
            Self::Pulls => "pulls?state=open&sort=updated&direction=desc&".into(),
            Self::Checks => format!(
                "commits/{}/check-runs?filter=latest&",
                query.head.unwrap_or_default()
            ),
            Self::Runs => format!("actions/runs?head_sha={}&", query.head.unwrap_or_default()),
        };
        format!("{base}/{endpoint}per_page=50&page={page}")
    }

    const fn recorded(self) -> bool {
        matches!(self, Self::Issues | Self::Pulls | Self::Checks | Self::Runs)
    }

    fn rows(self, value: &Value) -> Option<&Vec<Value>> {
        match self {
            Self::Checks => value.get("check_runs")?.as_array(),
            Self::Runs => value.get("workflow_runs")?.as_array(),
            Self::Contributors | Self::Collaborators | Self::Issues | Self::Pulls => {
                value.as_array()
            }
        }
    }
}
