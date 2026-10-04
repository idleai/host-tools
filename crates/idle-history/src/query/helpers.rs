use super::{ActivityKind, Error, Filter, Observation};
use editchain_core::{
    Op, OpId, OpKind,
    activity::{Kind, Operation},
};

/// Parse a complete canonical observation or item identity.
///
/// # Errors
/// Rejects shortened or noncanonical identities.
pub fn full_id(value: &str) -> Result<OpId, Error> {
    OpId::from_display_str(value)
        .filter(|id| value.len() == 64 && id.to_string() == value)
        .ok_or_else(|| error("Expected a complete canonical observation or item identity"))
}

pub(super) fn error(message: &str) -> Error {
    Error {
        message: message.to_owned(),
    }
}

impl Observation {
    /// Decode the full engine operation without using a lossy display adapter.
    ///
    /// # Errors
    /// Rejects malformed identities, mismatched envelopes and invalid activities.
    pub fn operation(&self) -> Result<Op, Error> {
        let id = full_id(&self.record.operation)?;
        let _hash = full_id(&self.record.hash)?;
        let op: Op = serde_json::from_slice(&self.operation_json)
            .map_err(|failure| error(&format!("Invalid operation: {failure}")))?;
        if op.id != id {
            return Err(error(
                "Operation ID differs from its stored-record reference",
            ));
        }
        if let OpKind::Activity(activity) = &op.kind {
            if !activity.matches_envelope(&op) {
                return Err(error("Activity differs from its recorded envelope"));
            }
            activity
                .validate()
                .map_err(|failure| error(&failure.to_string()))?;
        }
        Ok(op)
    }

    /// Stable item key using the engine's legacy read adapter where needed.
    ///
    /// # Errors
    /// Returns validation errors from [`Self::operation`].
    pub fn item_key(&self) -> Result<String, Error> {
        let op = self.operation()?;
        Ok(item_key(&op))
    }
}

/// Select the recorded logical item, falling back to the exact operation ID.
#[must_use]
pub fn item_key(op: &Op) -> String {
    Operation::view(op).map_or_else(|| op.id.to_string(), |activity| activity.item.to_string())
}

/// Classify a supported activity or retain an explicit legacy/unknown category.
#[must_use]
pub fn kind(op: &Op) -> ActivityKind {
    Operation::view(op).map_or_else(
        || {
            if matches!(op.kind, OpKind::ChainStart(_)) {
                ActivityKind::Initialization
            } else {
                ActivityKind::Unknown
            }
        },
        |operation| match operation.kind {
            Kind::Session(_) => ActivityKind::Session,
            Kind::Turn(_) => ActivityKind::Turn,
            Kind::Message(_) => ActivityKind::Message,
            Kind::Tool(_) => ActivityKind::Tool,
            Kind::File(_) => ActivityKind::File,
            Kind::Commit(_) => ActivityKind::Commit,
            Kind::Note(_) => ActivityKind::Note,
            Kind::Author(_) => ActivityKind::Author,
            Kind::Link(_) => ActivityKind::Link,
            Kind::Original(_) => ActivityKind::Original,
        },
    )
}

impl Filter {
    /// Match recorded facts after adapting legacy reads, with no inferred context.
    #[must_use]
    pub fn matches(&self, op: &Op) -> bool {
        let activity = Operation::view(op);
        (self.kinds.is_empty() || self.kinds.contains(&kind(op)))
            && self.session.as_ref().is_none_or(|id| {
                activity
                    .as_ref()
                    .and_then(|value| value.session)
                    .is_some_and(|value| value.to_string() == *id)
            })
            && self.author.as_ref().is_none_or(|id| {
                activity
                    .as_ref()
                    .and_then(|value| value.author)
                    .is_some_and(|value| value.to_string() == *id)
            })
            && self.recorder.as_ref().is_none_or(|id| {
                activity
                    .as_ref()
                    .is_some_and(|value| value.recorder.to_string() == *id)
            })
            && self.path.as_ref().is_none_or(|path| {
                activity.as_ref().is_some_and(|value| match &value.kind {
                    Kind::File(file) => file.path.0.to_string() == *path,
                    Kind::Commit(commit) => commit
                        .changed_paths
                        .iter()
                        .any(|id| id.0.to_string() == *path),
                    Kind::Session(_)
                    | Kind::Turn(_)
                    | Kind::Message(_)
                    | Kind::Tool(_)
                    | Kind::Note(_)
                    | Kind::Author(_)
                    | Kind::Link(_)
                    | Kind::Original(_) => false,
                })
            })
    }
}
