//! Versioned editor observations, independent of history-view snapshots.

use serde::{Deserialize, Serialize};

mod replay;

/// Maximum UTF-8 snapshot size; shared with the VS Code recorder's limit.
pub const MAX_EDITOR_BUFFER_BYTES: usize = 8 * 1024 * 1024;

/// One durable, replayable batch from a workspace recorder.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordEditorEvents {
    /// Absolute workspace root.
    pub workspace_path: String,
    /// Chain location, relative to the workspace or absolute.
    pub chain_dir: String,
    /// Ordered observations; identities remain unchanged on retry.
    pub events: Vec<EditorEvent>,
}

/// One observation in the `vscode.editor` source stream.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EditorEvent {
    /// Schema version, currently one.
    pub schema: u32,
    /// Explicit coordinate contract emitted by the current recorder.
    /// Older raw archives use the same units but omit this field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub units: Option<EditorUnits>,
    /// Random recorder incarnation, shared by all events until restart.
    pub session: String,
    /// Persistent local attribution, independent of the recorder incarnation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<idle_history::human::HumanIdentity>,
    /// Account display name observed locally; not a verified account binding.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_name: Option<String>,
    /// Strictly increasing, one-based identity within the incarnation.
    pub sequence: u64,
    /// Observer wall time. Duration measurements use a monotonic clock.
    pub time_ms: u64,
    /// Observation payload. Window focus is deliberately not recorded.
    pub event: EditorEventKind,
}

/// Native editor coordinates and snapshot encoding; values are not interchangeable.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EditorUnits {
    /// Replacement offsets and lengths, always `utf16_code_units`.
    pub offsets: String,
    /// Ranges, always `zero_based_line_utf16_column`.
    pub positions: String,
    /// Buffer snapshot content, always `utf8_bytes`.
    pub snapshots: String,
}

/// Exact identity of an observed buffer revision.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EditorDocument {
    /// Document incarnation, distinct across close/reopen and language changes.
    pub id: String,
    /// Full document URI, including the scheme.
    pub uri: String,
    /// Workspace-relative file path; absent for untitled buffers.
    pub path: Option<String>,
    /// VS Code buffer version.
    pub version: u64,
}

/// A zero-based half-open UTF-16 range, as emitted by VS Code.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EditorRange {
    /// Start line and UTF-16 column.
    pub start: [u32; 2],
    /// End line and UTF-16 column.
    pub end: [u32; 2],
}

/// One replacement in the original emitted order, against the evolving buffer.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EditorChange {
    /// Offset in UTF-16 code units, not Rust byte offsets.
    pub offset: u32,
    /// Replaced length in UTF-16 code units.
    pub length: u32,
    /// Inserted text.
    pub text: String,
}

/// Bounded mechanism details from VS Code's optional detailed-change API.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EditorChangeOrigin {
    /// Reported source, including unknown sources without a human claim.
    pub source: String,
    /// Cursor operation kind, when reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<String>,
    /// Input source or editor command, when reported.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detailed_source: Option<String>,
    /// Mechanism name, for example a formatting operation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// Reported provider extension, without a verified authorship claim.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extension_id: Option<String>,
}

impl EditorChangeOrigin {
    fn validate(&self) -> Result<(), &'static str> {
        for value in [
            Some(&self.source),
            self.kind.as_ref(),
            self.detailed_source.as_ref(),
            self.name.as_ref(),
            self.extension_id.as_ref(),
        ]
        .into_iter()
        .flatten()
        {
            if value.is_empty() || value.len() > 512 {
                return Err("invalid editor change origin");
            }
        }
        Ok(())
    }
}

/// An individual input receipt retained within a typing burst.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EditorEditAttribution {
    /// Earlier document-change sequence in this recorder incarnation.
    pub change: u64,
    /// Input details supporting this individual change.
    pub signal: String,
}

/// Stable editor observations and explicit human-work indicators.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum EditorEventKind {
    /// Independently observed Git context; this is not a workspace snapshot.
    WorkspaceContext {
        /// Observer wall time for the Git read.
        observed_ms: u64,
        /// Workspace root at observation time, never the later query location.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        workspace_path: Option<String>,
        /// Worktrees discovered within this workspace.
        repositories: Vec<idle_history::human::HumanGitContext>,
    },
    /// Recorder policy and runtime version.
    TrackingStarted {
        /// Minimum continuous exposure used as a reading indicator.
        dwell_ms: u64,
        /// VS Code version.
        vscode_version: String,
        /// Loaded recorder package version, absent in older recordings.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        extension_version: Option<String>,
        /// Activity derivation contract; absent retains the original work series.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        activity_schema: Option<u32>,
    },
    /// Normal recorder shutdown.
    TrackingStopped,
    /// An explicit capture limitation or missing interval.
    TrackingGap {
        /// Human-readable reason, without file contents.
        reason: String,
    },
    /// Initial or recovered buffer content.
    DocumentSnapshot {
        /// Revision identity.
        document: EditorDocument,
        /// Exact text, including unsaved changes.
        text: String,
    },
    /// An observed text change; authorship is a separate indicator.
    DocumentChanged {
        /// Destination revision.
        document: EditorDocument,
        /// Source revision number.
        before_version: u64,
        /// Exact source content.
        before: String,
        /// Exact destination content.
        after: String,
        /// Raw changes in emitted order.
        changes: Vec<EditorChange>,
        /// Stable reason: undo, redo, or absent.
        reason: Option<String>,
        /// Optional detailed mechanism; absence retains legacy observations.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        origin: Option<EditorChangeOrigin>,
    },
    /// Human intent inferred from editor input, keyboard selection, or undo/redo.
    HumanEdit {
        /// Earlier document-change sequence in this recorder incarnation.
        change: u64,
        /// Observable basis, not a verified author identity.
        signal: String,
    },
    /// One bounded typing burst, preserving each constituent input receipt.
    HumanEditBatch {
        /// First change in this live edit; subsequent receipts update its row.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        group: Option<u64>,
        /// Strict source order, without gaps in buffer revision continuity.
        edits: Vec<EditorEditAttribution>,
    },
    /// A visible edit whose source observations carry no human-input receipt.
    ObservedEditBatch {
        /// First source change in this continuously published edit.
        group: u64,
        /// Earlier changes, in strict source order and buffer continuity.
        changes: Vec<u64>,
    },
    /// Saved revision. An edit does not imply a save.
    DocumentSaved {
        /// Saved buffer identity.
        document: EditorDocument,
    },
    /// Explicit file or directory rename.
    DocumentRenamed {
        /// Previous workspace-relative path.
        from: String,
        /// Destination workspace-relative path.
        to: String,
    },
    /// A text tab opened; this alone does not indicate exposure.
    EditorOpened {
        /// Startup inventory describes an existing open file, not a new action.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        restored: Option<bool>,
        /// Tab identity, distinguishing split views.
        editor: String,
        /// Document URI.
        uri: String,
        /// Captured workspace-relative path; absent for legacy/untitled tabs.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<String>,
    },
    /// A text tab closed.
    EditorClosed {
        /// Previously opened tab identity.
        editor: String,
        /// Document URI.
        uri: String,
        /// Captured workspace-relative path; absent for legacy/untitled tabs.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        path: Option<String>,
    },
    /// The active text editor changed.
    EditorActivated {
        /// Active document, absent when no tracked editor is active.
        document: Option<EditorDocument>,
    },
    /// Legacy cursor or selection movement; new recorders keep this local.
    SelectionChanged {
        /// Exact buffer revision.
        document: EditorDocument,
        /// Visible editor identity.
        editor: String,
        /// Selections in API order.
        ranges: Vec<EditorRange>,
        /// Whether the API reported keyboard input.
        keyboard: bool,
    },
    /// Legacy viewport geometry; new recorders keep this local.
    VisibleRangesChanged {
        /// Exact buffer revision.
        document: EditorDocument,
        /// Visible editor identity.
        editor: String,
        /// Disjoint visible ranges; folded gaps remain excluded.
        ranges: Vec<EditorRange>,
    },
    /// Legacy exposure, retained for ingestion and replay of older recorders.
    CodeExposure {
        /// Exact buffer revision.
        document: EditorDocument,
        /// Visible editor identity.
        editor: String,
        /// Ranges visible throughout this interval.
        ranges: Vec<EditorRange>,
        /// Wall time at interval start.
        started_ms: u64,
        /// Monotonic elapsed duration, bounded by the recorder heartbeat.
        duration_ms: u64,
    },
    /// One reading indicator after a stable foreground view reaches its dwell threshold.
    CodeRead {
        /// Reading during an open edit is supporting details for that activity.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        group: Option<u64>,
        /// Exact buffer revision.
        document: EditorDocument,
        /// Visible editor identity.
        editor: String,
        /// Disjoint ranges visible throughout the qualifying interval.
        ranges: Vec<EditorRange>,
        /// Wall time at interval start.
        started_ms: u64,
        /// Monotonic duration at qualification, not the total time spent reading.
        duration_ms: u64,
    },
}

impl RecordEditorEvents {
    /// Validate capture identities, bounds and exact UTF-16 replay.
    /// # Errors
    /// Returns the first malformed event.
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.events.is_empty() || self.events.len() > 128 {
            return Err("editor batch must contain 1..128 events");
        }
        for event in &self.events {
            event.validate()?;
        }
        Ok(())
    }
}

impl EditorEvent {
    /// Validate one observation's identity, bounds and exact UTF-16 replacements.
    /// # Errors
    /// Returns the first malformed field or inconsistent buffer change.
    pub fn validate(&self) -> Result<(), &'static str> {
        let invalid = |message| message;
        if self.units.as_ref().is_some_and(|units| {
            units.offsets != "utf16_code_units"
                || units.positions != "zero_based_line_utf16_column"
                || units.snapshots != "utf8_bytes"
        }) {
            return Err("unsupported editor coordinate units");
        }
        if self
            .user_name
            .as_deref()
            .is_some_and(|name| !idle_history::human::valid_user_name(name))
        {
            return Err(invalid(
                "human user name must contain 1..80 characters without control characters or surrounding whitespace",
            ));
        }
        if let Some(identity) = &self.identity {
            let guid = identity.guid.as_bytes();
            if guid.len() != 36
                || !guid.iter().enumerate().all(|(index, byte)| {
                    if [8, 13, 18, 23].contains(&index) {
                        *byte == b'-'
                    } else {
                        byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()
                    }
                })
                || identity.stream.len() != 24
                || !identity
                    .stream
                    .bytes()
                    .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
            {
                return Err(invalid(
                    "invalid unsigned human identity or workspace stream",
                ));
            }
        }
        if self.schema != 1
            || self.sequence == 0
            || self.sequence > 9_007_199_254_740_991
            || self.session.len() != 36
            || !self
                .session
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() || byte == b'-')
        {
            return Err(invalid(
                "unsupported editor schema or invalid event identity",
            ));
        }
        if (self.sequence == 1) != matches!(self.event, EditorEventKind::TrackingStarted { .. }) {
            return Err(invalid("editor stream must begin with tracking_started"));
        }
        self.validate_content().map_err(invalid)?;
        if let EditorEventKind::HumanEdit { change, .. } = self.event
            && (change == 0 || change >= self.sequence)
        {
            return Err(invalid("human edit must refer to an earlier observation"));
        }
        Ok(())
    }

    fn validate_content(&self) -> Result<(), &'static str> {
        self.validate_document()?;
        if let EditorEventKind::CodeRead {
            group: Some(group), ..
        } = &self.event
            && (*group == 0 || *group >= self.sequence)
        {
            return Err("reading group must refer to an earlier edit");
        }
        match &self.event {
            EditorEventKind::WorkspaceContext {
                repositories,
                workspace_path,
                ..
            } => {
                if workspace_path
                    .as_ref()
                    .is_some_and(|root| !absolute_source_path(root))
                    || repositories.len() > 64
                    || repositories.iter().any(|repo| {
                        repo.repository.parse::<u64>().is_err()
                            || !absolute_source_path(&repo.root)
                            || repo.head.as_ref().is_some_and(|head| {
                                editchain_core::GitOid::from_hex(head).is_none()
                            })
                    })
                {
                    return Err("invalid recorded Git context");
                }
            }
            EditorEventKind::TrackingStarted {
                dwell_ms,
                activity_schema,
                extension_version,
                ..
            } => {
                if !(500..=30000).contains(dwell_ms) {
                    return Err("invalid reading dwell threshold");
                }
                if activity_schema.is_some_and(|schema| schema != 2 && schema != 3)
                    || (*activity_schema == Some(3)) != self.identity.is_some()
                {
                    return Err("unsupported editor activity schema");
                }
                if extension_version
                    .as_ref()
                    .is_some_and(|version| version.is_empty() || version.len() > 64)
                {
                    return Err("invalid recorder extension version");
                }
            }
            EditorEventKind::DocumentChanged {
                document,
                before_version,
                before,
                after,
                changes,
                reason,
                origin,
            } => {
                validate_snapshot(before)?;
                validate_snapshot(after)?;
                if let Some(origin) = origin {
                    origin.validate()?;
                }
                if *before_version >= document.version
                    || changes.is_empty()
                    || reason
                        .as_deref()
                        .is_some_and(|reason| !matches!(reason, "undo" | "redo"))
                {
                    return Err("invalid document change revision or reason");
                }
                replay::validate(before, after, changes)?;
            }
            EditorEventKind::HumanEdit { signal, .. } => {
                if !matches!(
                    signal.as_str(),
                    "editor_input" | "keyboard_selection" | "typing_correction" | "undo" | "redo"
                ) {
                    return Err("unknown human edit signal");
                }
            }
            EditorEventKind::HumanEditBatch { edits, group } => {
                if edits.is_empty() || edits.len() > 1024 {
                    return Err("human edit batch must contain 1..1024 changes");
                }
                if group.is_some_and(|group| {
                    group == 0 || edits.first().is_none_or(|edit| group > edit.change)
                }) {
                    return Err("human edit group must begin at an earlier input receipt");
                }
                let mut previous = 0;
                for edit in edits {
                    if edit.change <= previous || edit.change >= self.sequence {
                        return Err(
                            "human edit batch must reference earlier changes in source order",
                        );
                    }
                    if !matches!(
                        edit.signal.as_str(),
                        "editor_input" | "keyboard_selection" | "typing_correction"
                    ) {
                        return Err(
                            "human edit batch requires input signals; undo/redo remain separate",
                        );
                    }
                    previous = edit.change;
                }
            }
            EditorEventKind::ObservedEditBatch { changes, group } => {
                if changes.is_empty()
                    || changes.len() > 1024
                    || *group == 0
                    || changes.first().is_none_or(|first| group > first)
                    || changes.last().is_none_or(|last| *last >= self.sequence)
                    || changes
                        .iter()
                        .zip(changes.iter().skip(1))
                        .any(|(left, right)| left >= right)
                {
                    return Err("observed edit must reference earlier changes in source order");
                }
            }
            EditorEventKind::SelectionChanged { ranges, .. }
            | EditorEventKind::VisibleRangesChanged { ranges, .. }
            | EditorEventKind::CodeExposure { ranges, .. }
            | EditorEventKind::CodeRead { ranges, .. } => {
                for range in ranges {
                    if range.start > range.end
                        || range.end.into_iter().any(|value| {
                            usize::try_from(value)
                                .map_or(true, |value| value > MAX_EDITOR_BUFFER_BYTES)
                        })
                    {
                        return Err("invalid editor range");
                    }
                }
                if let EditorEventKind::CodeExposure { duration_ms, .. }
                | EditorEventKind::CodeRead { duration_ms, .. } = self.event
                    && duration_ms > 60000
                {
                    return Err("editor viewing interval exceeds duration bound");
                }
            }
            EditorEventKind::DocumentSnapshot { text, .. } => validate_snapshot(text)?,
            EditorEventKind::TrackingStopped
            | EditorEventKind::TrackingGap { .. }
            | EditorEventKind::DocumentSaved { .. }
            | EditorEventKind::DocumentRenamed { .. }
            | EditorEventKind::EditorOpened { .. }
            | EditorEventKind::EditorClosed { .. }
            | EditorEventKind::EditorActivated { .. } => {}
        }
        Ok(())
    }

    fn validate_document(&self) -> Result<(), &'static str> {
        let document = match &self.event {
            EditorEventKind::DocumentSnapshot { document, .. }
            | EditorEventKind::DocumentChanged { document, .. }
            | EditorEventKind::DocumentSaved { document }
            | EditorEventKind::SelectionChanged { document, .. }
            | EditorEventKind::VisibleRangesChanged { document, .. }
            | EditorEventKind::CodeExposure { document, .. }
            | EditorEventKind::CodeRead { document, .. } => Some(document),
            EditorEventKind::EditorActivated { document } => document.as_ref(),
            EditorEventKind::WorkspaceContext { .. }
            | EditorEventKind::TrackingStarted { .. }
            | EditorEventKind::TrackingStopped
            | EditorEventKind::TrackingGap { .. }
            | EditorEventKind::HumanEdit { .. }
            | EditorEventKind::HumanEditBatch { .. }
            | EditorEventKind::ObservedEditBatch { .. }
            | EditorEventKind::DocumentRenamed { .. }
            | EditorEventKind::EditorOpened { .. }
            | EditorEventKind::EditorClosed { .. } => None,
        };
        let path = if let EditorEventKind::EditorOpened { path, .. }
        | EditorEventKind::EditorClosed { path, .. } = &self.event
        {
            path.as_ref()
        } else {
            document.and_then(|document| document.path.as_ref())
        };
        if document.is_some_and(|document| document.id.is_empty() || document.id.len() > 256)
            || path.is_some_and(|path| {
                let parsed = std::path::Path::new(path);
                path.is_empty()
                    || path.len() > 4096
                    || parsed.is_absolute()
                    || parsed
                        .components()
                        .any(|part| part == std::path::Component::ParentDir)
            })
        {
            return Err("invalid editor document identity or workspace-relative path");
        }
        Ok(())
    }
}

fn validate_snapshot(text: &str) -> Result<(), &'static str> {
    if text.len() > MAX_EDITOR_BUFFER_BYTES {
        return Err("editor snapshot exceeds 8 MiB capture limit");
    }
    Ok(())
}

/// Source paths describe the recording host, which may use another platform.
pub(crate) fn absolute_source_path(value: &str) -> bool {
    if value.len() > 4096 || value.contains('\0') {
        return false;
    }
    if value.starts_with('/') {
        return true;
    }
    if let [drive, b':', b'\\' | b'/', ..] = value.as_bytes()
        && drive.is_ascii_alphabetic()
    {
        return true;
    }
    value.strip_prefix("\\\\").is_some_and(|rest| {
        let mut parts = rest.split(['\\', '/']);
        parts.next().is_some_and(|server| !server.is_empty())
            && parts.next().is_some_and(|share| !share.is_empty())
    })
}
