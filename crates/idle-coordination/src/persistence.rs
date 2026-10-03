//! Private, atomic saved state with an exclusive local service lifetime.

use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::Mutex,
};

use crate::{Error, Result};

/// Maximum private state size, checked before allocation and replacement.
pub const MAX_STATE_BYTES: usize = 16 * 1024 * 1024;

/// Host-supplied secret persistence. Compare-and-swap must be atomic and durable.
/// Implementations must isolate directories/accounts and protect bearer tokens.
pub trait Persistence: std::fmt::Debug + Send + Sync {
    /// Read a bounded private value, or report that it has never been stored.
    ///
    /// # Errors
    /// Returns storage failures without exposing stored values.
    fn load(&self, key: &str) -> Result<Option<Vec<u8>>>;

    /// Durably replace exactly the expected value; `None` deletes the key.
    ///
    /// # Errors
    /// Returns `Conflict` for a changed value, or an uncertain storage failure.
    fn compare_exchange(
        &self,
        key: &str,
        previous: Option<&[u8]>,
        replacement: Option<&[u8]>,
    ) -> Result<()>;
}

/// Native private directory. A process lock excludes competing service owners.
#[derive(Debug)]
pub struct FilePersistence {
    directory: PathBuf,
    guard: Mutex<File>,
}

impl FilePersistence {
    /// Open or create a private directory and hold its exclusive owner lock.
    /// Existing directory permissions must already restrict access to its owner.
    ///
    /// # Errors
    /// Returns `Busy` for another owner, or invalid permissions/storage failures.
    pub fn open(directory: impl AsRef<Path>) -> Result<Self> {
        let directory = directory.as_ref();
        if !directory.exists() {
            let mut builder = fs::DirBuilder::new();
            let _builder = builder.recursive(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt as _;
                let _builder = builder.mode(0o700);
            }
            builder.create(directory)?;
        }
        let metadata = fs::symlink_metadata(directory)?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(Error::Invalid);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            if metadata.permissions().mode() & 0o077 != 0 {
                return Err(Error::Forbidden);
            }
        }
        let mut options = OpenOptions::new();
        let _options = options.read(true).write(true).create(true).truncate(false);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            let _options = options.mode(0o600);
        }
        let lock_path = directory.join("owner.lock");
        if lock_path
            .symlink_metadata()
            .is_ok_and(|m| m.file_type().is_symlink())
        {
            return Err(Error::Forbidden);
        }
        let guard = options.open(lock_path)?;
        guard.try_lock().map_err(|_error| Error::Busy)?;
        Ok(Self {
            directory: directory.to_path_buf(),
            guard: Mutex::new(guard),
        })
    }

    fn path(&self, key: &str) -> Result<PathBuf> {
        if key.is_empty()
            || key.len() > 64
            || !key.bytes().all(|c| c.is_ascii_lowercase() || c == b'-')
        {
            return Err(Error::Invalid);
        }
        Ok(self.directory.join(format!("{key}.json")))
    }

    fn read(&self, key: &str) -> Result<Option<Vec<u8>>> {
        let path = self.path(key)?;
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error.into()),
        };
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err(Error::Forbidden);
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            if metadata.permissions().mode() & 0o077 != 0 {
                return Err(Error::Forbidden);
            }
        }
        let mut bytes = Vec::new();
        let _read = File::open(path)?
            .take(
                u64::try_from(MAX_STATE_BYTES)
                    .map_err(|_error| Error::Invalid)?
                    .saturating_add(1),
            )
            .read_to_end(&mut bytes)?;
        if bytes.len() > MAX_STATE_BYTES {
            return Err(Error::Invalid);
        }
        Ok(Some(bytes))
    }
}

impl Persistence for FilePersistence {
    fn load(&self, key: &str) -> Result<Option<Vec<u8>>> {
        let _guard = self.guard.lock().map_err(|_error| Error::Storage)?;
        self.read(key)
    }

    fn compare_exchange(
        &self,
        key: &str,
        previous: Option<&[u8]>,
        replacement: Option<&[u8]>,
    ) -> Result<()> {
        let _guard = self.guard.lock().map_err(|_error| Error::Storage)?;
        if self.read(key)?.as_deref() != previous {
            return Err(Error::Conflict);
        }
        let path = self.path(key)?;
        if let Some(bytes) = replacement {
            if bytes.len() > MAX_STATE_BYTES {
                return Err(Error::Invalid);
            }
            let mut temporary = tempfile::NamedTempFile::new_in(&self.directory)?;
            temporary.write_all(bytes)?;
            temporary.as_file().sync_all()?;
            let _file = temporary.persist(path).map_err(|_error| Error::Storage)?;
        } else if previous.is_some() {
            fs::remove_file(path)?;
        }
        #[cfg(unix)]
        File::open(&self.directory)?.sync_all()?;
        Ok(())
    }
}
