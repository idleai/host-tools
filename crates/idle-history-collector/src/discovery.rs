//! Source and Git discovery shared by native hosts.

use std::{
    collections::BTreeMap,
    fs,
    io::{self, Read as _},
    path::{Component, Path, PathBuf},
    time::SystemTime,
};

use crate::{Binding, Mode};

#[cfg(unix)]
use std::os::unix::fs::MetadataExt as _;

#[derive(Debug, Clone, Eq, PartialEq)]
pub(crate) struct Stamp {
    length: u64,
    modified: Option<SystemTime>,
    created: Option<SystemTime>,
    #[cfg(unix)]
    identity: (u64, u64, i64, i64),
}

pub(crate) type GitState = BTreeMap<PathBuf, Option<Stamp>>;

#[derive(Debug, Default)]
pub(crate) struct Sources {
    pub(crate) files: BTreeMap<PathBuf, Stamp>,
    pub(crate) titles: Option<Stamp>,
    pub(crate) git: GitState,
}

impl Sources {
    pub(crate) fn newest_first(&self) -> Vec<&PathBuf> {
        let mut files: Vec<_> = self.files.iter().collect();
        files.sort_by(|(left, left_stamp), (right, right_stamp)| {
            right_stamp
                .modified
                .cmp(&left_stamp.modified)
                .then_with(|| left.cmp(right))
        });
        files.into_iter().map(|(path, _stamp)| path).collect()
    }
}

pub(crate) fn capture(binding: &Binding, mode: Mode) -> io::Result<Sources> {
    let mut sources = Sources {
        git: workspace_git(&binding.workspace)?,
        ..Sources::default()
    };
    if mode == Mode::Import {
        sources.files = files(&binding.sessions, |path| {
            path.file_name().is_some_and(|name| {
                let name = name.to_string_lossy();
                name.starts_with("rollout-") && name.ends_with(".jsonl")
            })
        })?;
        sources.titles = stamp(&binding.sessions.join("session_index.jsonl"))?;
        if sources.titles.is_none()
            && binding
                .sessions
                .file_name()
                .is_some_and(|name| name == "sessions")
            && let Some(parent) = binding.sessions.parent()
        {
            sources.titles = stamp(&parent.join("session_index.jsonl"))?;
        }
    }
    Ok(sources)
}

fn stamp(path: &Path) -> io::Result<Option<Stamp>> {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    Ok(Some(Stamp {
        length: metadata.len(),
        modified: metadata.modified().ok(),
        created: metadata.created().ok(),
        #[cfg(unix)]
        identity: (
            metadata.dev(),
            metadata.ino(),
            metadata.ctime(),
            metadata.ctime_nsec(),
        ),
    }))
}

fn files(root: &Path, accepts: impl Fn(&Path) -> bool) -> io::Result<BTreeMap<PathBuf, Stamp>> {
    let mut found = BTreeMap::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let entries = match fs::read_dir(directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        for entry in entries {
            let entry = entry?;
            let kind = entry.file_type()?;
            let path = entry.path();
            if kind.is_dir() {
                pending.push(path);
            } else if kind.is_file()
                && accepts(&path)
                && let Some(stamp) = stamp(&path)?
            {
                let _previous = found.insert(path, stamp);
            }
        }
    }
    Ok(found)
}

fn git_directory(marker: &Path) -> io::Result<PathBuf> {
    if marker.is_file() {
        let pointer = fs::read_to_string(marker)?;
        let value = pointer
            .trim()
            .strip_prefix("gitdir: ")
            .ok_or_else(|| io::Error::other("invalid Git directory pointer"))?;
        return Ok(marker
            .parent()
            .ok_or_else(|| io::Error::other("Git marker has no parent"))?
            .join(value));
    }
    Ok(marker.to_path_buf())
}

fn git_state(marker: &Path, state: &mut GitState) -> io::Result<()> {
    let git = git_directory(marker)?;
    let common = match fs::read_to_string(git.join("commondir")) {
        Ok(pointer) => git.join(pointer.trim()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => git.clone(),
        Err(error) => return Err(error),
    };
    for (path, stamp) in files(&common.join("refs"), |path| {
        path.extension().is_none_or(|extension| extension != "lock")
    })? {
        let _previous = state.insert(path, Some(stamp));
    }
    for path in [
        git.join("HEAD"),
        common.join("packed-refs"),
        common.join("shallow"),
    ] {
        let version = stamp(&path)?;
        let _previous = state.insert(path, version);
    }
    Ok(())
}

fn workspace_git(workspace: &Path) -> io::Result<GitState> {
    let mut state = GitState::new();
    let mut pending = vec![workspace.to_path_buf()];
    while let Some(directory) = pending.pop() {
        let entries = fs::read_dir(&directory)?.collect::<io::Result<Vec<_>>>()?;
        if entries.iter().any(|entry| entry.file_name() == ".git") {
            git_state(&directory.join(".git"), &mut state)?;
        } else if directory.join("HEAD").is_file()
            && directory.join("objects").is_dir()
            && directory.join("refs").is_dir()
        {
            git_state(&directory, &mut state)?;
            continue;
        }
        for entry in entries {
            let name = entry.file_name();
            if entry.file_type()?.is_dir()
                && !name.to_string_lossy().starts_with('.')
                && name != "target"
                && name != "node_modules"
            {
                pending.push(entry.path());
            }
        }
    }
    Ok(state)
}

pub(crate) fn belongs_to_workspace(file: &Path, workspace: &Path) -> io::Result<bool> {
    let mut bytes = Vec::new();
    let _read = fs::File::open(file)?.take(65_536).read_to_end(&mut bytes)?;
    let Some(line) = bytes.split(|byte| *byte == b'\n').next() else {
        return Ok(true);
    };
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(line) else {
        return Ok(true);
    };
    if value.get("type").and_then(serde_json::Value::as_str) != Some("session_meta") {
        return Ok(true);
    }
    let Some(cwd) = value
        .get("payload")
        .and_then(|payload| payload.get("cwd"))
        .and_then(serde_json::Value::as_str)
        .map(Path::new)
        .filter(|path| path.is_absolute())
    else {
        return Ok(true);
    };
    let cwd = fs::canonicalize(cwd).unwrap_or_else(|_| normalize(cwd));
    Ok(cwd.starts_with(fs::canonicalize(workspace)?))
}

fn normalize(path: &Path) -> PathBuf {
    let mut result = PathBuf::new();
    for component in path.components() {
        match component {
            Component::ParentDir => {
                let _removed = result.pop();
            }
            Component::CurDir => {}
            Component::Normal(_) | Component::RootDir | Component::Prefix(_) => {
                result.push(component.as_os_str());
            }
        }
    }
    result
}

#[cfg(test)]
#[path = "discovery_tests.rs"]
mod tests;
