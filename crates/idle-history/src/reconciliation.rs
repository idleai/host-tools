//! Validation of ephemeral revisioned snapshot updates, never durable cursors.

/// Server-local revision state. A new connection always supplies a new baseline.
#[derive(Clone, Debug)]
pub struct Revisions<E> {
    epoch: E,
    revision: u64,
}

/// A revision stream needs an authoritative baseline before it can continue.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReconcileError {
    /// The source generation changed.
    EpochChanged,
    /// A required revision is absent or an update skips a revision.
    Gap,
    /// The envelope does not describe the supplied revisions.
    Incomplete,
}

impl<E: PartialEq> Revisions<E> {
    /// Start at a server-issued baseline, not an operation ID or refresh count.
    pub const fn new(epoch: E, revision: u64) -> Self {
        Self { epoch, revision }
    }

    /// Source-local epoch, valid only for this baseline's connection.
    #[must_use]
    pub const fn epoch(&self) -> &E {
        &self.epoch
    }

    /// Last applied source-local revision, never a durable resume checkpoint.
    #[must_use]
    pub const fn revision(&self) -> u64 {
        self.revision
    }

    /// Validate one advancing delta after a complete envelope was planned.
    ///
    /// # Errors
    /// Returns a gap if the delta does not immediately follow accepted state.
    pub fn next(&self, base: u64, revision: u64) -> Result<(), ReconcileError> {
        if base != self.revision || base.checked_add(1) != Some(revision) {
            return Err(ReconcileError::Gap);
        }
        Ok(())
    }

    /// Validate the whole replay without changing the accepted revision.
    /// Returns the last new delta's index, or `None` for a duplicate replay.
    ///
    /// # Errors
    /// A different epoch, revision gap or inconsistent final revision requires recovery.
    pub fn plan(
        &self,
        epoch: &E,
        through: u64,
        deltas: impl IntoIterator<Item = (u64, u64)>,
    ) -> Result<Option<usize>, ReconcileError> {
        if self.epoch != *epoch {
            return Err(ReconcileError::EpochChanged);
        }
        let mut revision = self.revision;
        let mut latest = None;
        let mut last = None;
        for (index, (base, next)) in deltas.into_iter().enumerate() {
            if base.checked_add(1) != Some(next) || last.is_some_and(|last| next <= last) {
                return Err(ReconcileError::Gap);
            }
            last = Some(next);
            if next <= self.revision {
                continue;
            }
            if base != revision {
                return Err(ReconcileError::Gap);
            }
            revision = next;
            latest = Some(index);
        }
        if last.is_some_and(|last| last != through)
            || (through > self.revision && revision != through)
        {
            return Err(ReconcileError::Incomplete);
        }
        Ok(latest)
    }

    /// Commit only after the corresponding replacement view has been validated.
    pub fn commit(&mut self, revision: u64) {
        self.revision = self.revision.max(revision);
    }
}

#[cfg(test)]
mod tests {
    use super::{ReconcileError, Revisions};

    #[test]
    fn reordered_duplicate_envelopes_do_not_rewind_the_view() {
        let mut state = Revisions::new("source", 2);
        assert_eq!(
            state.plan(&"source", 4, [(1, 2), (2, 3), (3, 4)]),
            Ok(Some(2)),
            "replay overlap"
        );
        state.commit(4);
        assert_eq!(
            state.plan(&"source", 3, [(1, 2), (2, 3)]),
            Ok(None),
            "late duplicate batch"
        );
        assert_eq!(
            state.plan(&"source", 6, [(5, 6)]),
            Err(ReconcileError::Gap),
            "lost update"
        );
        assert_eq!(
            state.plan(&"other", 5, [(4, 5)]),
            Err(ReconcileError::EpochChanged),
            "new source"
        );
        assert_eq!(
            state.plan(&"source", 6, [(4, 5)]),
            Err(ReconcileError::Incomplete),
            "incomplete batch"
        );
    }
}
