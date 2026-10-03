use std::sync::{Arc, Mutex};

use serde::{Deserialize, Serialize};

use crate::{Error, Result, invitation::HostLease, persistence::Persistence};

const KEY: &str = "relay-journal";

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct Entry {
    pub marker: String,
    pub lease: Option<HostLease>,
    pub created_at: u64,
}

#[derive(Debug)]
pub(super) struct Journal {
    storage: Arc<dyn Persistence>,
    serial: Mutex<()>,
}

impl Journal {
    pub(super) fn new(storage: Arc<dyn Persistence>) -> Self {
        Self {
            storage,
            serial: Mutex::new(()),
        }
    }

    pub(super) fn entries(&self) -> Result<Vec<Entry>> {
        let bytes = self.storage.load(KEY)?;
        bytes.map_or_else(
            || Ok(Vec::new()),
            |bytes| Ok(serde_json::from_slice(&bytes)?),
        )
    }

    pub(super) fn remember(&self, entry: &Entry) -> Result<()> {
        self.edit(|entries| {
            if let Some(previous) = entries
                .iter_mut()
                .find(|previous| previous.marker == entry.marker)
            {
                if previous.lease.is_some()
                    && entry.lease.is_some()
                    && previous.lease != entry.lease
                {
                    return Err(Error::Conflict);
                }
                if entry.lease.is_some() {
                    previous.lease.clone_from(&entry.lease);
                }
            } else {
                if entries.len() >= 64 {
                    return Err(Error::Busy);
                }
                entries.push(entry.clone());
            }
            Ok(())
        })
    }

    pub(super) fn forget(&self, marker: &str) -> Result<()> {
        self.edit(|entries| {
            entries.retain(|entry| entry.marker != marker);
            Ok(())
        })
    }

    fn edit(&self, action: impl Fn(&mut Vec<Entry>) -> Result<()>) -> Result<()> {
        let _guard = self.serial.lock().map_err(|_error| Error::Storage)?;
        let previous = self.storage.load(KEY)?;
        let mut entries: Vec<Entry> = previous
            .as_ref()
            .map_or_else(|| Ok(Vec::new()), |bytes| serde_json::from_slice(bytes))?;
        action(&mut entries)?;
        let bytes = serde_json::to_vec(&entries)?;
        self.storage
            .compare_exchange(KEY, previous.as_deref(), Some(&bytes))
    }
}
