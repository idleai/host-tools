//! Immutable first-parent file lists and exact descriptor-bound text reads.

use std::{io, path::Path};

use editchain_core::GitOid;
use editchain_git::{
    RepositoryHandle,
    diff::{GitFileChange, GitFileStatus},
};
use idle_history::{
    query::QueryResult,
    timeline::{CommitFile, Target},
};

use crate::history::service::Binding;

pub(crate) fn changes(root: &Path, repository: &str, oid: &str) -> io::Result<Vec<CommitFile>> {
    let handle = repository_handle(root, repository, oid)?;
    let oid_value =
        GitOid::from_hex(oid).ok_or_else(|| io::Error::other("Invalid commit hash."))?;
    let _commit = editchain_git::resolve_commit(&handle, &oid_value).map_err(io::Error::other)?;
    editchain_git::diff::commit_file_changes(&handle, &oid_value)
        .map_err(io::Error::other)
        .map(|files| {
            files
                .iter()
                .map(|file| descriptor(repository, oid, file))
                .collect()
        })
}

fn repository_handle(root: &Path, repository: &str, oid: &str) -> io::Result<RepositoryHandle> {
    if !(Target::Commit {
        repository: repository.into(),
        oid: oid.into(),
    })
    .is_valid()
    {
        return Err(io::Error::other("Invalid commit destination."));
    }
    let handle = super::open(root)?;
    if handle.discovery.id.0.to_string() != repository {
        return Err(io::Error::other(
            "The commit belongs to a different repository.",
        ));
    }
    Ok(handle)
}

fn descriptor(repository: &str, oid: &str, file: &GitFileChange) -> CommitFile {
    CommitFile {
        repository: repository.into(),
        oid: oid.into(),
        path: file.path.clone(),
        old_path: file.old_path.clone(),
        old_oid: file.old_oid.map(|oid| oid.to_string()),
        new_oid: file.new_oid.map(|oid| oid.to_string()),
        old_mode: file.old_mode.clone(),
        new_mode: file.new_mode.clone(),
        status: match file.status {
            GitFileStatus::Added => "A",
            GitFileStatus::Deleted => "D",
            GitFileStatus::Modified => "M",
            GitFileStatus::Renamed => "R",
            GitFileStatus::Copied => "C",
            GitFileStatus::TypeChanged => "T",
        }
        .into(),
        binary: file.binary,
    }
}

pub(crate) fn document(binding: &Binding, change: &CommitFile) -> io::Result<QueryResult> {
    if !change.is_valid() {
        return Err(io::Error::other("Invalid Git file destination."));
    }
    let root = binding
        .repository_directory
        .as_ref()
        .ok_or_else(|| io::Error::other("This history connection has no bound Git checkout."))?;
    if !changes(root, &change.repository, &change.oid)?.contains(change) {
        return Err(io::Error::other(
            "The requested file change does not match the recorded commit.",
        ));
    }
    let handle = repository_handle(root, &change.repository, &change.oid)?;
    let (before, after) = if change.binary {
        (String::new(), String::new())
    } else {
        (
            text(
                &handle,
                change.old_oid.as_deref(),
                change.old_mode.as_deref(),
            )?,
            text(
                &handle,
                change.new_oid.as_deref(),
                change.new_mode.as_deref(),
            )?,
        )
    };
    Ok(QueryResult::CommitFile {
        change: Box::new(change.clone()),
        before,
        after,
    })
}

fn text(handle: &RepositoryHandle, oid: Option<&str>, mode: Option<&str>) -> io::Result<String> {
    let Some(oid) = oid else {
        return Ok(String::new());
    };
    if mode == Some("commit") {
        return Ok(format!("Subproject commit {oid}\n"));
    }
    let oid = GitOid::from_hex(oid).ok_or_else(|| io::Error::other("Invalid Git blob hash."))?;
    let blob = editchain_git::diff::resolve_blob(handle, &oid).map_err(io::Error::other)?;
    if blob.binary || blob.bytes.len() > 8 * 1024 * 1024 {
        return Err(io::Error::other(
            "The Git file cannot be represented within the text document limit.",
        ));
    }
    String::from_utf8(blob.bytes).map_err(io::Error::other)
}
