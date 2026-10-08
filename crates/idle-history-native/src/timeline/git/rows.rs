//! Live commit descriptions and exact Git destinations.

use editchain_core::{GitCommitEntity, Payload};
use idle_history::{
    ContentText,
    query::{ActivityKind, OpenTarget},
    timeline::{Geometry, Row, Target},
};

use super::super::{model::Fact, presentation};

pub(super) fn fact(commit: &GitCommitEntity, refs: Vec<String>) -> (Fact, String) {
    let repository = commit.repository.0.to_string();
    let oid = commit.oid.to_string();
    let id = format!("git:{repository}:{oid}");
    let message = text(&commit.message);
    let (preview, tag) = presentation::commit(&message);
    let mut fact = Fact {
        row: Row {
            occurrence: id.clone(),
            item: id,
            records: Vec::new(),
            address: Target::Commit {
                repository: repository.clone(),
                oid: oid.clone(),
            },
            kind: ActivityKind::Commit,
            title: "git".into(),
            preview: ContentText::new(preview, true).text,
            author: text(&commit.author.name),
            session: String::new(),
            tags: tag.into_iter().collect(),
            timestamp: u64::try_from(commit.committed_at)
                .ok()
                .and_then(|value| value.checked_mul(1000)),
            open: OpenTarget::Diff,
            unavailable: None,
            group: None,
            relationships: Vec::new(),
            graph: Geometry::default(),
        },
        parents: Vec::new(),
        causes: Vec::new(),
        original: None,
        aliases: Vec::new(),
        author: None,
        session: None,
        recorder: format!("git:{repository}"),
        order_time: None,
        task: None,
        task_title: None,
        attempt: None,
        path: None,
        source: None,
        raw_hash: None,
        provider: None,
        link: None,
        git: Some((
            format!("{repository}:{oid}"),
            commit
                .parents
                .iter()
                .map(|parent| format!("{repository}:{parent}"))
                .collect(),
        )),
        label: None,
        terminal: false,
        protected: true,
        outcome: editchain_core::activity::Status::Unknown,
        searchable: true,
        visibility: super::super::model::Visibility::Primary,
        supports: Vec::new(),
        form: super::super::model::RecordForm::Projected,
        human_edit: None,
        note_turn: None,
    };
    labels(&mut fact, refs);
    let search = format!(
        "{message}\n{}\n{}\n{oid}",
        fact.row.author,
        text(&commit.author.email)
    )
    .to_lowercase();
    (fact, search)
}

pub(super) fn labels(fact: &mut Fact, labels: Vec<String>) {
    fact.row.tags.retain(|tag| !tag.starts_with("ref: "));
    fact.row
        .tags
        .extend(labels.into_iter().map(|label| format!("ref: {label}")));
}

fn text(payload: &Payload) -> String {
    if let Payload::Inline(bytes) = payload {
        String::from_utf8_lossy(bytes).into_owned()
    } else {
        String::new()
    }
}
