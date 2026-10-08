//! Late records, explicit typed links, and repository-qualified attachments.

use std::io;

use editchain_core::{
    GitAvailability, GitCommitEntity, GitObjectFormat, GitOid, GitSignature, Op, Payload,
    RepositoryId,
    activity::{Author, AuthorRole, Entity, ItemId, Kind, Link, Operation},
};
use editchain_engine::Engine;
use idle_history::timeline::{RelationshipKind, Source};

use super::{binding, id, key, latest, message, row};

fn operation(value: u64, kind: Kind) -> io::Result<Op> {
    Operation::new(id(value), ItemId(id(value)), ItemId(id(900)), kind)
        .into_op()
        .map_err(io::Error::other)
}

fn link(value: u64, child: u64, parent: Entity, relation: &str) -> io::Result<Op> {
    operation(
        value,
        Kind::Link(Link {
            from: Entity::Operation(id(child)),
            relation: relation.into(),
            to: vec![parent],
            content: Payload::Empty,
        }),
    )
}

#[test]
fn an_unavailable_git_base_preserves_the_preceding_task_boundary() -> io::Result<()> {
    let directory = tempfile::tempdir()?;
    let engine = Engine::open(directory.path())?;
    for value in 1_u64..=5 {
        let parents = value
            .checked_sub(1)
            .filter(|value| *value != 0)
            .into_iter()
            .collect::<Vec<_>>();
        let mut activity = Operation::view(&message(value, 800, value, &parents)?)
            .ok_or_else(|| io::Error::other("fixture activity"))?;
        activity.turn = Some(ItemId(id(700)));
        let _admitted = engine.append(&activity.into_op().map_err(io::Error::other)?)?;
    }
    let binding = binding(directory.path());
    check!(
        row(&latest(&binding)?, 3)?.group.is_some(),
        "an uninterrupted task may be summarized"
    );
    let commit = Entity::Git {
        repository: RepositoryId(42),
        oid: GitOid::from_hex(&"a".repeat(40)).ok_or_else(|| io::Error::other("commit fixture"))?,
    };
    let _admitted = engine.append(&link(100, 4, commit, "BasedOn")?)?;
    let window = latest(&binding)?;
    equal!(
        window.activities,
        5,
        "an unavailable commit does not fabricate an activity"
    );
    check!(
        row(&window, 3)?.group.is_none(),
        "the predecessor of a recorded Git base remains explicit"
    );
    check!(
        row(&window, 4)?
            .relationships
            .iter()
            .any(|relation| relation.kind == RelationshipKind::Git
                && relation.parent.is_none()
                && relation.unresolved.is_some()),
        "the missing base remains inspectable"
    );
    Ok(())
}

#[test]
fn late_logical_parent_and_author_keep_identity_across_recorder_restarts() -> io::Result<()> {
    let directory = tempfile::tempdir()?;
    let engine = Engine::open(directory.path())?;
    let mut child = Operation::view(&message(2, 800, 20, &[])?)
        .ok_or_else(|| io::Error::other("fixture activity"))?;
    child.author = Some(ItemId(id(700)));
    child.time_ms = None;
    let _admitted = engine.append(&child.into_op().map_err(io::Error::other)?)?;
    for (value, relation) in [(3, "ProviderParent"), (4, "LogicalParent")] {
        let _admitted = engine.append(&link(value, 2, Entity::Item(ItemId(id(1))), relation)?)?;
    }
    let binding = binding(directory.path());
    let first = latest(&binding)?;
    check!(
        row(&first, 2)?
            .relationships
            .iter()
            .all(|relation| relation.parent.is_none()),
        "unavailable logical targets cannot create a connection"
    );
    equal!(
        row(&first, 2)?.timestamp,
        None,
        "unrecorded time remains absent"
    );
    let mut parent = Operation::view(&message(1, 800, 40, &[])?)
        .ok_or_else(|| io::Error::other("fixture activity"))?;
    parent.author = Some(ItemId(id(700)));
    parent.recorder = ItemId(id(901));
    let _admitted = engine.append(&parent.into_op().map_err(io::Error::other)?)?;
    let mut label = Operation::view(&operation(
        5,
        Kind::Author(Author {
            label: Payload::Inline(b"Recorded person".to_vec()),
            role: AuthorRole::Person,
            native_role: Payload::Empty,
            metadata: Payload::Empty,
        }),
    )?)
    .ok_or_else(|| io::Error::other("fixture author"))?;
    label.item = ItemId(id(700));
    let _admitted = engine.append(&label.into_op().map_err(io::Error::other)?)?;
    let updated = latest(&binding)?;
    for value in [1, 2] {
        equal!(
            &row(&updated, value)?.author,
            "Recorded person",
            "author labels survive recorder changes"
        );
    }
    equal!(
        &row(&updated, 1)?.session,
        &row(&updated, 2)?.session,
        "one recorded session survives restart"
    );
    equal!(
        &row(&updated, 2)?.graph.parents,
        &vec![key(Source::Current, id(1))],
        "late typed links repair one shared route"
    );
    check!(
        row(&updated, 2)?
            .relationships
            .iter()
            .all(|relation| relation.unresolved.is_none()),
        "late parent resolves both recorded links"
    );
    equal!(
        updated.rows.first().map(|row| row
            .address
            .record()
            .expect("record destination")
            .record
            .operation
            .clone()),
        Some(id(2).to_string()),
        "causal order wins over missing and skewed times"
    );
    Ok(())
}

fn oid(byte: u8) -> io::Result<GitOid> {
    GitOid::new(GitObjectFormat::Sha256, [byte; 32])
        .ok_or_else(|| io::Error::other("fixture Git object"))
}

fn commit(value: u64, repository: u64, object: u8, parents: Vec<GitOid>) -> io::Result<Op> {
    let signature = GitSignature {
        name: Payload::Empty,
        email: Payload::Empty,
        when: 0,
    };
    operation(
        value,
        Kind::Commit(Box::new(GitCommitEntity {
            repository: RepositoryId(repository),
            object_format: GitObjectFormat::Sha256,
            oid: oid(object)?,
            imported_record: None,
            availability: GitAvailability::ImportedOnly,
            tree: oid(99)?,
            parents,
            author: signature.clone(),
            committer: signature,
            authored_at: 0,
            committed_at: 0,
            message: Payload::Inline(b"recorded commit".to_vec()),
            imported_refs: Vec::new(),
            live_refs: Vec::new(),
            changed_paths: Vec::new(),
        })),
    )
}

#[test]
fn git_duplicates_share_qualified_anchors_and_late_bases_repair_links() -> io::Result<()> {
    let directory = tempfile::tempdir()?;
    let engine = Engine::open(directory.path())?;
    for op in [
        commit(1, 10, 1, Vec::new())?,
        commit(2, 20, 1, Vec::new())?,
        commit(3, 10, 2, vec![oid(1)?])?,
        commit(4, 20, 2, vec![oid(1)?])?,
        message(5, 800, 10, &[])?,
        link(
            6,
            5,
            Entity::Git {
                repository: RepositoryId(30),
                oid: oid(1)?,
            },
            "based_on",
        )?,
        commit(7, 10, 1, Vec::new())?,
    ] {
        let _admitted = engine.append(&op)?;
    }
    let binding = binding(directory.path());
    let first = latest(&binding)?;
    let qualified = [key(Source::Current, id(1)), key(Source::Current, id(7))];
    check!(
        row(&first, 3)?
            .graph
            .parents
            .iter()
            .all(|parent| qualified.contains(parent)),
        "same hashes in other repositories cannot become parents"
    );
    equal!(
        row(&first, 3)?.graph.parents.len(),
        1,
        "duplicate imports share one attachment anchor"
    );
    equal!(
        &row(&first, 4)?.graph.parents,
        &vec![key(Source::Current, id(2))],
        "second repository uses its own base"
    );
    check!(row(&first, 5)?.relationships.iter().any(|relation| relation.kind == RelationshipKind::Git && relation.unresolved.is_some()), "missing repository base stays unresolved");
    let _admitted = engine.append(&commit(8, 30, 1, Vec::new())?)?;
    let updated = latest(&binding)?;
    equal!(
        &row(&updated, 5)?.graph.parents,
        &vec![key(Source::Current, id(8))],
        "late base repairs its exact activity attachment"
    );
    Ok(())
}

#[test]
fn late_content_and_conflicts_update_the_existing_snapshot() -> io::Result<()> {
    let directory = tempfile::tempdir()?;
    let elsewhere = tempfile::tempdir()?;
    let engine = Engine::open(directory.path())?;
    let bytes = b"late searchable payload";
    let reference = Engine::open(elsewhere.path())?.store_blob(bytes)?;
    let parent = message(1, 800, 10, &[])?;
    let mut child = Operation::view(&message(2, 800, 20, &[1])?)
        .ok_or_else(|| io::Error::other("fixture activity"))?;
    if let Kind::Message(message) = &mut child.kind {
        let block = message
            .blocks
            .first_mut()
            .ok_or_else(|| io::Error::other("fixture block"))?;
        block.content = Payload::Blob(reference);
    }
    for op in [parent.clone(), child.into_op().map_err(io::Error::other)?] {
        let _admitted = engine.append(&op)?;
    }
    let binding = binding(directory.path());
    let first = latest(&binding)?;
    check!(
        !row(&first, 2)?.preview.contains("searchable"),
        "unavailable bytes cannot become a preview"
    );
    let _stored = engine.store_blob(bytes)?;
    let available = latest(&binding)?;
    check!(
        row(&available, 2)?.preview.contains("searchable"),
        "late blob arrival refreshes compact content"
    );
    check!(
        first.revision != available.revision,
        "new content publishes a new coherent revision"
    );
    let result = super::request(
        &binding,
        super::Action::Find {
            view: super::View::default(),
            text: "searchable payload".into(),
            cursor: None,
            limit: 200,
        },
    )?;
    let super::Response::Found(found) = result else {
        return Err(io::Error::other("expected indexed Find result"));
    };
    equal!(found.total, 1, "new text participates in indexed Find");
    let mut conflict =
        Operation::view(&parent).ok_or_else(|| io::Error::other("fixture parent"))?;
    if let Kind::Message(message) = &mut conflict.kind {
        let block = message
            .blocks
            .first_mut()
            .ok_or_else(|| io::Error::other("fixture block"))?;
        block.content = Payload::Inline(b"conflicting recorded content".to_vec());
    }
    let _admitted = engine.append(&conflict.into_op().map_err(io::Error::other)?)?;
    let retracted = latest(&binding)?;
    check!(
        retracted.rows.iter().all(|row| row
            .address
            .record()
            .expect("record destination")
            .record
            .operation
            != id(1).to_string()),
        "conflicted parent leaves accepted display occurrences"
    );
    check!(
        row(&retracted, 2)?.graph.parents.is_empty(),
        "retraction removes the route to the disputed parent"
    );
    check!(
        row(&retracted, 2)?
            .relationships
            .iter()
            .any(|relation| relation.unresolved.is_some()),
        "the child retains its unresolved parent record"
    );
    Ok(())
}
