use std::{collections::BTreeSet, error::Error, fmt};

use super::{
    ProjectionAvailability, ProjectionInput, ProjectionKind, ProjectionReference,
    ProjectionSnapshot,
};

/// Invalid shared projection input. No part of the snapshot should be admitted.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InvalidProjection(pub &'static str);

impl fmt::Display for InvalidProjection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.0)
    }
}

impl Error for InvalidProjection {}

impl ProjectionReference {
    /// Check that complete identities remain separate and internally consistent.
    ///
    /// # Errors
    /// Rejects empty references, shortened/noncanonical IDs and unbound digests.
    pub fn validate(&self) -> Result<(), InvalidProjection> {
        if self.observation.is_none() && (self.item.is_none() || self.record_hash.is_some()) {
            return Err(InvalidProjection(
                "A reference needs an observation or item; a digest needs an observation",
            ));
        }
        for id in self
            .observation
            .iter()
            .chain(&self.item)
            .chain(&self.record_hash)
        {
            if id.len() != 64
                || !id
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            {
                return Err(InvalidProjection(
                    "History references require full lowercase 256-bit identities",
                ));
            }
        }
        Ok(())
    }
}

impl ProjectionInput {
    /// Validate counts, row identities, references and explicit limitations.
    ///
    /// # Errors
    /// Rejects inconsistent counts/availability, duplicate keys or invalid links.
    pub fn validate(&self) -> Result<(), InvalidProjection> {
        let count = u64::try_from(self.rows.len())
            .map_err(|_error| InvalidProjection("Too many projection rows"))?;
        if self.total.is_some_and(|total| total.0 < count) {
            return Err(InvalidProjection(
                "Projection total is smaller than its supplied rows",
            ));
        }
        match self.availability {
            ProjectionAvailability::Complete
                if self.total.map(|total| total.0) != Some(count) || !self.gaps.is_empty() =>
            {
                return Err(InvalidProjection(
                    "Complete projections require an exact count and no gaps",
                ));
            }
            ProjectionAvailability::Unavailable
                if !self.rows.is_empty() || self.total.is_some() =>
            {
                return Err(InvalidProjection(
                    "Unavailable projections cannot claim rows or a total",
                ));
            }
            ProjectionAvailability::Partial | ProjectionAvailability::Unavailable
                if self.gaps.is_empty() =>
            {
                return Err(InvalidProjection(
                    "Partial and unavailable projections must explain their limitations",
                ));
            }
            ProjectionAvailability::Complete
            | ProjectionAvailability::Partial
            | ProjectionAvailability::Unavailable => {}
        }
        if self
            .freshness
            .checkpoint
            .as_ref()
            .is_some_and(String::is_empty)
        {
            return Err(InvalidProjection(
                "An absent checkpoint must be omitted, not empty",
            ));
        }
        let mut keys = BTreeSet::new();
        for row in &self.rows {
            if row.key.is_empty() || !keys.insert(&row.key) || row.sources.is_empty() {
                return Err(InvalidProjection(
                    "Projection rows require unique nonempty keys and source references",
                ));
            }
            if row
                .sources
                .iter()
                .any(|source| source.observation.is_none())
            {
                return Err(InvalidProjection(
                    "Projection row sources require observation references",
                ));
            }
            for reference in row.sources.iter().chain(&row.related) {
                reference.validate()?;
            }
        }
        for gap in &self.gaps {
            if gap.message.is_empty() {
                return Err(InvalidProjection(
                    "Projection limitations need an explanation",
                ));
            }
            if let Some(reference) = &gap.reference {
                reference.validate()?;
            }
        }
        Ok(())
    }
}

impl ProjectionSnapshot {
    /// Validate the atomic input before making any part visible to clients.
    ///
    /// # Errors
    /// Rejects missing scope, missing/duplicate destinations or invalid inputs.
    pub fn validate(&self) -> Result<(), InvalidProjection> {
        if self.workspace_id.0.is_empty() || self.chain.0.is_empty() {
            return Err(InvalidProjection(
                "Projection snapshots require a workspace and logical chain",
            ));
        }
        let kinds: BTreeSet<_> = self.inputs.iter().map(|input| input.kind).collect();
        if self.inputs.len() != ProjectionKind::ALL.len()
            || kinds.len() != ProjectionKind::ALL.len()
        {
            return Err(InvalidProjection(
                "Projection snapshots require each of the five destinations exactly once",
            ));
        }
        self.inputs.iter().try_for_each(ProjectionInput::validate)
    }
}
