use std::collections::BTreeSet;

use idle_protocol::v1::{
    identity::{ProjectionCount, Timestamp},
    projections::{
        FreshnessStatus, ProjectionAvailability, ProjectionFreshness, ProjectionGap,
        ProjectionInput, ProjectionKind, ProjectionReference, ProjectionRow,
    },
    repository::{GithubPerson, GithubRepository, ReadState},
};
use serde_json::Value;
use url::Url;

use super::Dataset;

pub(super) fn repository(value: &Value) -> Option<GithubRepository> {
    Some(GithubRepository {
        full_name: text(value, "full_name", 220)?,
        url: source_url(value)?,
        description: value
            .get("description")
            .and_then(Value::as_str)
            .map(|value| crate::short_text(value, 500)),
        default_branch: text(value, "default_branch", 256)?,
        visibility: text(value, "visibility", 32)?,
    })
}

pub(super) fn people(dataset: &mut Dataset) -> Vec<GithubPerson> {
    let mut ids = BTreeSet::new();
    let mut people = Vec::new();
    for (value, _) in &dataset.rows {
        let person = (|| {
            Some(GithubPerson {
                id: value.get("id")?.as_u64()?.to_string(),
                login: text(value, "login", 100)?,
                url: source_url(value)?,
                contributions: value
                    .get("contributions")
                    .and_then(Value::as_u64)
                    .and_then(|count| u32::try_from(count).ok()),
                role: value
                    .get("role_name")
                    .and_then(Value::as_str)
                    .map(|role| crate::short_text(role, 100)),
            })
        })();
        if let Some(person) = person
            && ids.insert(person.id.clone())
        {
            people.push(person);
        } else {
            dataset.report.state = ReadState::Partial;
            dataset.report.message = "GitHub returned duplicate or incomplete account entries; only distinct valid entries are shown.".into();
        }
    }
    people
}

pub(super) fn inputs(
    issues: &Dataset,
    pulls: &Dataset,
    checks: &Dataset,
    runs: &Dataset,
    now: u64,
) -> Vec<ProjectionInput> {
    let mut tasks = Vec::new();
    let mut malformed = false;
    for (data, pull) in [(issues, false), (pulls, true)] {
        for (value, source) in &data.rows {
            let Some(row) = source
                .as_ref()
                .and_then(|source| issue(value, source, pull))
                .filter(|row| serde_json::to_vec(row).is_ok_and(|bytes| bytes.len() <= 8192))
            else {
                malformed = true;
                continue;
            };
            if let Some(existing) = tasks
                .iter_mut()
                .find(|existing: &&mut ProjectionRow| existing.key == row.key)
            {
                *existing = row;
            } else {
                tasks.push(row);
            }
        }
    }
    let triage = tasks
        .iter()
        .filter(|row| {
            row.labels.iter().any(|label| {
                matches!(
                    label.to_ascii_lowercase().as_str(),
                    "triage" | "needs-triage" | "needs triage"
                )
            })
        })
        .cloned()
        .collect();
    let need_input = tasks
        .iter()
        .filter(|row| {
            row.labels.iter().any(|label| {
                matches!(
                    label.to_ascii_lowercase().as_str(),
                    "needs-input"
                        | "needs input"
                        | "needs:input"
                        | "needs-human-input"
                        | "review requested"
                )
            })
        })
        .cloned()
        .collect();
    let sources = [issues, pulls];
    let mut inputs = vec![
        input(ProjectionKind::Task, tasks, &sources, now, malformed),
        input(ProjectionKind::Triage, triage, &sources, now, malformed),
        input(
            ProjectionKind::NeedInput,
            need_input,
            &sources,
            now,
            malformed,
        ),
    ];
    let mut errors = Vec::new();
    let mut malformed = false;
    for (data, kind) in [(checks, "check"), (runs, "workflow run")] {
        for (value, source) in &data.rows {
            let Some(conclusion) = value.get("conclusion") else {
                malformed = true;
                continue;
            };
            if !matches!(
                conclusion.as_str(),
                Some("failure" | "timed_out" | "startup_failure")
            ) {
                continue;
            }
            if let Some(row) = source
                .as_ref()
                .and_then(|source| error(value, source, kind))
            {
                if !errors
                    .iter()
                    .any(|existing: &ProjectionRow| existing.key == row.key)
                {
                    errors.push(row);
                }
            } else {
                malformed = true;
            }
        }
    }
    inputs.push(input(
        ProjectionKind::Error,
        errors,
        &[checks, runs],
        now,
        malformed,
    ));
    inputs
}

fn issue(value: &Value, source: &ProjectionReference, pull: bool) -> Option<ProjectionRow> {
    let url = source_url(value)?;
    let number = value.get("number")?.as_u64()?;
    let title = text(value, "title", 500)?;
    let mut labels = value
        .get("labels")?
        .as_array()?
        .iter()
        .map(|label| text(label, "name", 128))
        .collect::<Option<Vec<_>>>()?;
    let is_pull = pull || value.get("pull_request").is_some();
    let mut summary = format!(
        "GitHub {} · updated {}",
        if is_pull { "pull request" } else { "issue" },
        text(value, "updated_at", 64)?
    );
    if pull {
        let reviewers = value
            .get("requested_reviewers")?
            .as_array()?
            .iter()
            .map(|person| text(person, "login", 100))
            .collect::<Option<Vec<_>>>()?;
        let teams = value
            .get("requested_teams")?
            .as_array()?
            .iter()
            .map(|team| text(team, "slug", 100))
            .collect::<Option<Vec<_>>>()?;
        if !reviewers.is_empty() || !teams.is_empty() {
            labels.push("Review requested".into());
            let requested = reviewers
                .into_iter()
                .chain(teams.into_iter().map(|team| format!("team/{team}")))
                .take(10)
                .collect::<Vec<_>>()
                .join(", ");
            summary.push_str(" · reviewers: ");
            summary.push_str(&requested);
        }
    }
    Some(ProjectionRow {
        key: format!("github:{url}"),
        title: format!("#{number} {title}"),
        summary: Some(summary),
        url: Some(url),
        status: Some(text(value, "state", 32)?),
        labels,
        sources: vec![source.clone()],
        related: Vec::new(),
    })
}

fn error(value: &Value, source: &ProjectionReference, kind: &str) -> Option<ProjectionRow> {
    let id = value.get("id")?.as_u64()?;
    Some(ProjectionRow {
        key: format!("github:{kind}:{id}"),
        title: text(value, "name", 500)?,
        summary: Some(format!("GitHub {kind} for this checkout's HEAD")),
        url: Some(source_url(value)?),
        status: Some(text(value, "conclusion", 64)?),
        labels: vec![format!("GitHub {kind}")],
        sources: vec![source.clone()],
        related: Vec::new(),
    })
}

fn input(
    kind: ProjectionKind,
    rows: Vec<ProjectionRow>,
    sources: &[&Dataset],
    now: u64,
    malformed: bool,
) -> ProjectionInput {
    let mut gaps = sources
        .iter()
        .filter(|data| data.report.state != ReadState::Complete)
        .map(|data| ProjectionGap {
            reference: None,
            message: format!("{}: {}", data.report.topic, data.report.message),
        })
        .collect::<Vec<_>>();
    if malformed {
        gaps.push(ProjectionGap { reference: None, message: "GitHub returned incomplete rows or rows beyond the 8-KiB presentation limit. Only bounded rows with exact stored sources and valid fields are shown.".into() });
    }
    let availability = if gaps.is_empty() {
        ProjectionAvailability::Complete
    } else if rows.is_empty()
        && sources.iter().all(|data| {
            matches!(
                data.report.state,
                ReadState::Unavailable | ReadState::NotApplicable
            )
        })
    {
        ProjectionAvailability::Unavailable
    } else {
        ProjectionAvailability::Partial
    };
    let total = (availability == ProjectionAvailability::Complete)
        .then(|| ProjectionCount(u64::try_from(rows.len()).unwrap_or(u64::MAX)));
    ProjectionInput {
        kind,
        freshness: ProjectionFreshness {
            status: if availability == ProjectionAvailability::Complete {
                FreshnessStatus::Current
            } else {
                FreshnessStatus::Unknown
            },
            generated_at: Some(Timestamp(now)),
            checkpoint: None,
        },
        availability,
        total,
        rows,
        gaps,
    }
}

pub(super) fn unavailable(now: u64, message: &str) -> Vec<ProjectionInput> {
    [
        ProjectionKind::Task,
        ProjectionKind::Error,
        ProjectionKind::Triage,
        ProjectionKind::NeedInput,
    ]
    .into_iter()
    .map(|kind| ProjectionInput {
        kind,
        freshness: ProjectionFreshness {
            status: FreshnessStatus::Unknown,
            generated_at: Some(Timestamp(now)),
            checkpoint: None,
        },
        availability: ProjectionAvailability::Unavailable,
        total: None,
        rows: Vec::new(),
        gaps: vec![ProjectionGap {
            reference: None,
            message: message.into(),
        }],
    })
    .collect()
}

fn text(value: &Value, key: &str, bound: usize) -> Option<String> {
    let text = value.get(key)?.as_str()?;
    (!text.is_empty()).then(|| crate::short_text(text, bound))
}

fn source_url(value: &Value) -> Option<String> {
    let raw = value.get("html_url")?.as_str()?;
    let url = Url::parse(raw).ok()?;
    (raw.len() <= 2048
        && url.scheme() == "https"
        && url.host_str() == Some("github.com")
        && url.port().is_none()
        && url.username().is_empty()
        && url.password().is_none())
    .then(|| url.to_string())
}
