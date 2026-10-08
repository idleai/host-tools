use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::Path,
};

use serde::{Deserialize, Serialize};

use crate::{Error, Result};

use super::RepositoryFiles;

const MAX_FILE_BYTES: u64 = 1_048_576;
const MAX_TOTAL_BYTES: usize = 4 * 1_048_576;
const MAX_PROJECTIONS: usize = 256;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(crate) struct FileWrite {
    pub name: String,
    pub before: Option<String>,
    pub after: String,
}

impl RepositoryFiles {
    pub(super) fn validate_directories(&self) -> Result<()> {
        let parent = self.directory.parent().ok_or(Error::Invalid)?;
        for path in [
            parent.to_path_buf(),
            self.directory.clone(),
            self.directory.join("projections"),
        ] {
            match fs::symlink_metadata(path) {
                Ok(meta) if !meta.is_dir() || meta.file_type().is_symlink() => {
                    return Err(Error::Invalid);
                }
                Ok(_) => (),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => (),
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }

    pub(crate) fn read_file(&self, name: &str) -> Result<Option<String>> {
        self.validate_directories()?;
        validate_name(name)?;
        let path = self.directory.join(name);
        let meta = match fs::symlink_metadata(&path) {
            Ok(meta) => meta,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        if !meta.is_file() || meta.file_type().is_symlink() || meta.len() > MAX_FILE_BYTES {
            return Err(Error::Invalid);
        }
        let mut bytes = Vec::new();
        let _count = File::open(path)?
            .take(MAX_FILE_BYTES.saturating_add(1))
            .read_to_end(&mut bytes)?;
        if u64::try_from(bytes.len()).map_err(|_error| Error::Invalid)? > MAX_FILE_BYTES {
            return Err(Error::Invalid);
        }
        Ok(Some(
            String::from_utf8(bytes).map_err(|_error| Error::Invalid)?,
        ))
    }

    pub(super) fn documents(&self) -> Result<BTreeMap<String, String>> {
        let mut documents = BTreeMap::new();
        for name in [
            "workspace.json",
            "settings.json",
            "agent-rules.json",
            "control.json",
            "hosts.json",
            "providers.json",
        ] {
            if let Some(content) = self.read_file(name)? {
                let _old = documents.insert(name.into(), content);
                check_limits(&documents)?;
            }
        }
        let directory = self.directory.join("projections");
        if directory.exists() {
            let mut count = 0usize;
            for entry in fs::read_dir(directory)? {
                let entry = entry?;
                if entry
                    .path()
                    .extension()
                    .is_none_or(|extension| extension != "json")
                {
                    continue;
                }
                count = count.saturating_add(1);
                if count > MAX_PROJECTIONS {
                    return Err(Error::Invalid);
                }
                let name = format!(
                    "projections/{}",
                    entry.file_name().to_str().ok_or(Error::Invalid)?
                );
                let content = self.read_file(&name)?.ok_or(Error::Conflict)?;
                let _old = documents.insert(name, content);
                check_limits(&documents)?;
            }
        }
        Ok(documents)
    }

    pub(crate) fn lock(&self) -> Result<File> {
        let digest = blake3::hash(self.directory.as_os_str().as_encoded_bytes());
        let path = std::env::temp_dir().join(format!("idle-workspace-{digest}.lock"));
        if fs::symlink_metadata(&path)
            .is_ok_and(|meta| !meta.is_file() || meta.file_type().is_symlink())
        {
            return Err(Error::Invalid);
        }
        let mut options = OpenOptions::new();
        let _options = options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            let _options = options.mode(0o600);
        }
        let file = options.open(path)?;
        file.try_lock().map_err(|_error| Error::Busy)?;
        Ok(file)
    }

    pub(crate) fn finish_write(&self, write: &FileWrite) -> Result<()> {
        let current = self.read_file(&write.name)?;
        if current.as_ref() == Some(&write.after) {
            return Ok(());
        }
        if current != write.before {
            return Err(Error::Conflict);
        }
        self.validate_directories()?;
        let path = self.directory.join(&write.name);
        let parent = path.parent().ok_or(Error::Invalid)?;
        fs::create_dir_all(parent)?;
        self.validate_directories()?;
        let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
        temporary.write_all(write.after.as_bytes())?;
        temporary.as_file().sync_all()?;
        // Recheck after preparing the replacement so an editor save is detected.
        if self.read_file(&write.name)? != write.before {
            return Err(Error::Conflict);
        }
        let _saved = temporary.persist(&path).map_err(|_error| Error::Storage)?;
        sync_directory(parent)
    }
}

pub(super) fn check_limits(documents: &BTreeMap<String, String>) -> Result<()> {
    if documents
        .keys()
        .filter(|name| name.starts_with("projections/"))
        .count()
        > MAX_PROJECTIONS
        || documents
            .values()
            .any(|text| u64::try_from(text.len()).map_or(true, |size| size > MAX_FILE_BYTES))
        || documents
            .values()
            .try_fold(0usize, |total, text| total.checked_add(text.len()))
            .is_none_or(|total| total > MAX_TOTAL_BYTES)
    {
        return Err(Error::Invalid);
    }
    Ok(())
}

fn validate_name(name: &str) -> Result<()> {
    if matches!(
        name,
        "workspace.json"
            | "settings.json"
            | "agent-rules.json"
            | "control.json"
            | "hosts.json"
            | "providers.json"
    ) {
        return Ok(());
    }
    let id = name
        .strip_prefix("projections/")
        .and_then(|name| name.strip_suffix(".json"))
        .ok_or(Error::Invalid)?;
    let encoded = id.strip_prefix('~').is_some_and(|hash| {
        hash.len() == 64
            && hash
                .bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
    });
    if encoded || super::validation::projection_path(id)? == name {
        Ok(())
    } else {
        Err(Error::Invalid)
    }
}

fn sync_directory(path: &Path) -> Result<()> {
    #[cfg(unix)]
    File::open(path)?.sync_all()?;
    #[cfg(not(unix))]
    let _path = path;
    Ok(())
}
