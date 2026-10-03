//! Preserve successful command-to-commit relationships across imported formats.

use std::{
    collections::{BTreeSet, HashMap, HashSet},
    io,
    path::Path,
};

use editchain_core::{
    GitOid, ImportOp, Op, OpId, OpKind, Payload, RepositoryId,
    activity::{Entity, ItemId, Kind, Link, Operation, OriginalRef},
};
use editchain_git::{RepositoryCatalog, RepositoryHandle, open_repository, resolve_commit_prefix};
use editchain_store::CanonicalChain;
use idle_history_import::{
    FsBlobSink,
    git_evidence::{CommitEvidence, collect_commit_evidence},
};

pub(crate) fn derive(
    workspace: &Path,
    chain: &Path,
    staged: &[Op],
    revisit: bool,
) -> io::Result<Vec<Op>> {
    let blobs = FsBlobSink::open_read_only(chain.join("blobs"))?;
    let incoming = staged.iter().filter_map(source_record).collect::<Vec<_>>();
    if !revisit
        && !collect_commit_evidence(&incoming, blobs.as_ref())
            .iter()
            .any(CommitEvidence::invokes_git_commit)
    {
        return Ok(Vec::new());
    }
    let catalog = RepositoryCatalog::discover(workspace)?;
    if !catalog.is_complete() {
        return Err(io::Error::other("incomplete Git relationship catalog"));
    }
    let repositories = catalog
        .entries()
        .iter()
        .map(|entry| open_repository(entry).map_err(|error| io::Error::other(error.to_string())))
        .collect::<io::Result<Vec<_>>>()?;
    if repositories.is_empty() {
        return Ok(Vec::new());
    }
    let mut corpus = CanonicalChain::read(chain)?;
    for op in staged {
        let _admission = corpus.insert(op.clone())?;
    }
    let records = corpus
        .into_located_ops()
        .map(|(op, _)| op)
        .collect::<Vec<_>>();
    let aliases: HashMap<_, _> = records
        .iter()
        .filter_map(|op| {
            let OpKind::Activity(record) = &op.kind else {
                return None;
            };
            record
                .legacy
                .as_ref()
                .map(|mapping| (mapping.operation, record.id))
        })
        .collect();
    let mut existing = HashSet::new();
    for op in &records {
        if let Some(record) = Operation::view(op)
            && let Kind::Link(link) = record.kind
            && link.relation == "produced_by"
            && let Entity::Operation(source) = link.from
        {
            for target in link.to {
                if let Entity::Git { repository, oid } = target {
                    let _inserted = existing.insert((
                        *aliases.get(&source).unwrap_or(&source),
                        repository,
                        oid,
                    ));
                }
            }
        }
    }
    let originals = records.iter().filter_map(source_record).collect::<Vec<_>>();
    let mut links = Vec::new();
    for item in collect_commit_evidence(&originals, blobs.as_ref()) {
        if !item.invokes_git_commit() {
            continue;
        }
        for prefix in item.prefixes {
            let Some((repository, oid)) = unique_commit(&repositories, &prefix) else {
                continue;
            };
            if existing.insert((item.source.id, repository, oid)) {
                let source = records
                    .iter()
                    .find(|op| op.id == item.source.id)
                    .and_then(Operation::view)
                    .ok_or_else(|| io::Error::other("missing imported source"))?;
                links.push(link(&source, repository, oid)?);
            }
        }
    }
    Ok(links)
}

fn source_record(op: &Op) -> Option<Op> {
    if matches!(op.kind, OpKind::Import(_)) {
        return Some(op.clone());
    }
    let OpKind::Activity(record) = &op.kind else {
        return None;
    };
    let Kind::Original(raw) = &record.kind else {
        return None;
    };
    let mapping = record.legacy.as_ref()?;
    // Decode the retained input using the shared provider adapter. Its address
    // stays the current full record ID, so the resulting link resolves directly.
    Some(Op {
        id: record.id,
        source: mapping.source,
        parents: op.parents.clone(),
        actor: mapping.actor,
        clock: mapping.clock,
        scope: mapping.scope,
        tags: mapping.tags,
        kind: OpKind::Import(ImportOp {
            raw_ref: raw.bytes.clone(),
            raw_hash: raw.hash,
        }),
    })
}

fn unique_commit(
    repositories: &[RepositoryHandle],
    prefix: &str,
) -> Option<(RepositoryId, GitOid)> {
    let mut matches = BTreeSet::new();
    for repository in repositories {
        if let Some(commit) = resolve_commit_prefix(repository, prefix).ok()? {
            let _inserted = matches.insert((repository.discovery.id, commit.oid));
        }
    }
    (matches.len() == 1)
        .then(|| matches.into_iter().next())
        .flatten()
}

fn link(source: &Operation, repository: RepositoryId, oid: GitOid) -> io::Result<Op> {
    let key = format!("{}:{}:{}", source.id, repository.0, oid.to_hex());
    let id = OpId::from_bytes(blake3::derive_key(
        "idle.import.git-produced-by.v1",
        key.as_bytes(),
    ));
    let mut record = Operation::new(
        id,
        ItemId::derive("git-produced-link", id.as_bytes()),
        ItemId::derive("recorder", b"idle.history-collector"),
        Kind::Link(Link {
            from: Entity::Operation(source.id),
            relation: "produced_by".into(),
            to: vec![Entity::Git { repository, oid }],
            content: Payload::Empty,
        }),
    );
    record.parents.push(source.id);
    record.session = source.session;
    record.turn = source.turn;
    record.original = Some(OriginalRef {
        operation: source.id,
        converter: "git-produced-by-v1".into(),
    });
    record.into_op().map_err(io::Error::other)
}
