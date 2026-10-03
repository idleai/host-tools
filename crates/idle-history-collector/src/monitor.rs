//! An independent append frontier and unresolved content, unaffected by other readers.

use std::{collections::HashMap, io, path::Path};

use editchain_core::{Op, activity::Operation};
use editchain_engine::queries::{ContentReference, ContentState};
use editchain_store::{BlobReader, BlobResolution, IndexedTail};

#[derive(Debug)]
pub(crate) struct Monitor {
    tail: IndexedTail,
    pending: HashMap<ContentReference, ContentState>,
}

impl Monitor {
    pub(crate) fn new(root: &Path) -> io::Result<Self> {
        let tail = if root.is_dir() {
            IndexedTail::open(root)?
        } else {
            IndexedTail::empty(root)
        };
        let mut monitor = Self {
            tail,
            pending: HashMap::new(),
        };
        let blobs = BlobReader::open(root)?;
        for op in monitor.tail.chain().shared_ops().collect::<Vec<_>>() {
            monitor.observe(&op, &blobs)?;
        }
        Ok(monitor)
    }

    pub(crate) fn poll(&mut self, root: &Path) -> io::Result<bool> {
        if !root.exists() {
            return Ok(false);
        }
        let delta = self.tail.drain()?;
        let blobs = BlobReader::open(root)?;
        for (op, _) in delta.added.values() {
            self.observe(op, &blobs)?;
        }
        let mut changed = !delta.added.is_empty() || !delta.removed.is_empty();
        for reference in self.pending.keys().copied().collect::<Vec<_>>() {
            let state = state(reference, &blobs)?;
            changed |= self.pending.get(&reference) != Some(&state);
            if state == ContentState::Available {
                let _old = self.pending.remove(&reference);
            } else {
                let _old = self.pending.insert(reference, state);
            }
        }
        Ok(changed)
    }

    fn observe(&mut self, op: &Op, blobs: &BlobReader) -> io::Result<()> {
        if let Some(record) = Operation::view(op) {
            for (id, len) in record.kind.content_addresses() {
                let reference = ContentReference { id, len };
                let state = state(reference, blobs)?;
                if state != ContentState::Available {
                    let _old = self.pending.insert(reference, state);
                }
            }
        }
        Ok(())
    }
}

fn state(reference: ContentReference, blobs: &BlobReader) -> io::Result<ContentState> {
    Ok(match reference.resolve(blobs)? {
        BlobResolution::Found(_) => ContentState::Available,
        BlobResolution::Missing => ContentState::Missing,
        BlobResolution::Corrupt => ContentState::Corrupt,
        BlobResolution::Unresolvable => ContentState::Unresolvable,
    })
}
