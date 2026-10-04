use std::collections::BTreeSet;

use editchain_engine::{
    activity::{
        AuthorRole, ChangeState, Field, File, FileAction, ItemId, Kind, KindName, Note, NoteKind,
        Operation, TextRange,
    },
    queries::{ChainQueries, ContentField, HistoryEntry, IndexKey, Lookup},
};
use idle_editor_capture::wire::EditorEventKind;

use crate::history::{Failure, FailureCode, reference};

use super::{Indicator, IndicatorKind, Preview, Request, SourceRecord, ranges, records};

const MAX_INDICATORS: usize = 512;

pub(super) fn prepare(queries: &ChainQueries, request: &Request) -> Result<Preview, Failure> {
    let entry = records::selected(queries, &request.selection)?;
    let operation = Operation::view(&entry.operation).ok_or_else(|| {
        Failure::new(
            FailureCode::Unavailable,
            "This record has no file revision.",
        )
    })?;
    let Kind::File(file) = &operation.kind else {
        return Err(Failure::new(
            FailureCode::Unavailable,
            "This record is not a file observation.",
        ));
    };
    if file.change == Some(ChangeState::Proposed) {
        return Err(Failure::new(
            FailureCode::Unavailable,
            "A proposed change cannot describe an observed editor revision.",
        ));
    }
    let text = records::text(queries, operation.id, ContentField::FileAfter)?;
    if text.contains('\0') {
        return Err(Failure::new(
            FailureCode::Unavailable,
            "Binary snapshots have no text decorations.",
        ));
    }
    let mut projection = Projection {
        queries,
        entry: &entry,
        operation: &operation,
        file,
        result: Preview {
            request: request.clone(),
            record: reference(entry.record_ref),
            revision: file.revision.map(|revision| revision.to_string()),
            text,
            indicators: Vec::new(),
            issues: Vec::new(),
        },
    };
    projection.project()?;
    Ok(projection.result)
}

struct Projection<'a> {
    queries: &'a ChainQueries,
    entry: &'a HistoryEntry,
    operation: &'a Operation,
    file: &'a File,
    result: Preview,
}

impl Projection<'_> {
    fn project(&mut self) -> Result<(), Failure> {
        let stats = self.queries.index().stats();
        if stats.quarantined > 0 {
            self.issue("History contains quarantined records; observations may be incomplete.");
        }
        if stats.undecodable > 0 || stats.incomplete_tails > 0 {
            self.issue("Some history records are unreadable or incomplete.");
        }
        if self.file.revision.is_none() {
            self.issue(
                "No revision identity was recorded; exposure cannot be joined to this snapshot.",
            );
        }
        // Missing parents are reported without inventing continuity across the gap.
        for parent in &self.operation.parents {
            if !matches!(self.queries.operation(*parent)?, Lookup::Found(_)) {
                self.issue("A parent observation is missing or conflicted.");
            }
        }
        let edit_ranges = match ranges::edits(
            self.queries,
            self.operation.id,
            self.file,
            &self.result.text,
        ) {
            Ok(ranges) => ranges,
            Err(error) => {
                self.issue(&error.message);
                Vec::new()
            }
        };
        if let Some(author) = self.operation.author.filter(|_| {
            matches!(
                self.file.action,
                FileAction::Create | FileAction::Change | FileAction::Snapshot
            )
        }) {
            let (kind, label, mut sources) = self.author(author)?;
            sources.insert(0, records::source(self.entry));
            self.add(kind, &label, &edit_ranges, &sources);
        }
        if matches!(self.file.action, FileAction::Create | FileAction::Change) {
            self.notes(&edit_ranges)?;
        } else {
            self.notes(&[])?;
        }
        if !self.result.indicators.iter().any(|indicator| {
            matches!(
                indicator.kind,
                IndicatorKind::Human | IndicatorKind::Ai | IndicatorKind::Other
            )
        }) {
            self.add(
                IndicatorKind::Unknown,
                "Author not established for this revision",
                &[],
                &[records::source(self.entry)],
            );
        }
        if self
            .result
            .indicators
            .iter()
            .any(|indicator| indicator.kind == IndicatorKind::Human)
            && self
                .result
                .indicators
                .iter()
                .any(|indicator| indicator.kind == IndicatorKind::Ai)
        {
            self.issue("Both human and agent attribution are recorded; no single author classification is selected.");
            for indicator in &mut self.result.indicators {
                if matches!(indicator.kind, IndicatorKind::Human | IndicatorKind::Ai) {
                    indicator.kind = IndicatorKind::Unknown;
                }
            }
        }
        self.exposures()?;
        if !self.result.indicators.iter().any(|indicator| {
            matches!(
                indicator.kind,
                IndicatorKind::Exposure | IndicatorKind::Read
            )
        }) {
            self.issue("No exposure observations are available for this revision; this does not mean it was unread.");
        }
        Ok(())
    }

    fn scan(&mut self, key: IndexKey) -> Result<Vec<HistoryEntry>, Failure> {
        let (entries, complete) = records::scan(self.queries, key)?;
        if !complete {
            self.issue("The history query limit was reached; additional observations may exist.");
        }
        Ok(entries)
    }

    fn author(
        &mut self,
        author: ItemId,
    ) -> Result<(IndicatorKind, String, Vec<SourceRecord>), Failure> {
        let entries = self.scan(IndexKey::Item(author))?;
        let mut kinds = BTreeSet::new();
        let mut labels = BTreeSet::new();
        let mut sources = Vec::new();
        for entry in entries {
            let Some(operation) = Operation::view(&entry.operation) else {
                continue;
            };
            let Kind::Author(value) = operation.kind else {
                continue;
            };
            let _inserted = kinds.insert(match value.role {
                AuthorRole::Person => IndicatorKind::Human,
                AuthorRole::Agent => IndicatorKind::Ai,
                AuthorRole::Tool | AuthorRole::System => IndicatorKind::Other,
                AuthorRole::Unknown => IndicatorKind::Unknown,
            });
            if sources.len() < 32 {
                sources.push(records::source(&entry));
                match records::text(
                    self.queries,
                    operation.id,
                    ContentField::Record(Field::Label),
                ) {
                    Ok(label) => {
                        let _inserted = labels.insert(display(&label));
                    }
                    Err(error) => self.issue(&error.message),
                }
            } else {
                self.issue("Additional author registrations are available in history.");
            }
        }
        let kind = if kinds.len() == 1 {
            kinds.first().copied().unwrap_or(IndicatorKind::Unknown)
        } else {
            self.issue("Author registrations are missing or disagree.");
            IndicatorKind::Unknown
        };
        let role = match kind {
            IndicatorKind::Human => "Human author recorded",
            IndicatorKind::Ai => "AI author recorded",
            IndicatorKind::Other => "Tool or system author recorded",
            IndicatorKind::Unknown | IndicatorKind::Exposure | IndicatorKind::Read => {
                "Author role unknown"
            }
        };
        let label = if labels.is_empty() {
            format!("{role}: {author}")
        } else {
            format!(
                "{role}: {}",
                display(&labels.into_iter().collect::<Vec<_>>().join(" / "))
            )
        };
        Ok((kind, label, sources))
    }

    fn notes(&mut self, edit_ranges: &[TextRange]) -> Result<(), Failure> {
        for entry in self.scan(IndexKey::Kind(KindName::Note))? {
            let Some(operation) = Operation::view(&entry.operation) else {
                continue;
            };
            let Kind::Note(note) = &operation.kind else {
                continue;
            };
            let targeted = note.targets.contains(&self.operation.id);
            let session_gap = self.operation.session.is_some()
                && operation.session == self.operation.session
                && note.category == NoteKind::Gap;
            if !targeted && !session_gap {
                continue;
            }
            if note.category == NoteKind::Gap {
                self.issue(
                    "A capture gap was recorded in this session; its affected ranges are unknown.",
                );
                self.add(
                    IndicatorKind::Unknown,
                    "Capture gap recorded",
                    &[],
                    &[records::source(&entry)],
                );
            } else if targeted {
                self.receipt(&entry, &operation, note, edit_ranges)?;
            }
        }
        Ok(())
    }

    fn receipt(
        &mut self,
        entry: &HistoryEntry,
        operation: &Operation,
        note: &Note,
        ranges: &[TextRange],
    ) -> Result<(), Failure> {
        let code = match records::text(
            self.queries,
            operation.id,
            ContentField::Record(Field::Code),
        ) {
            Ok(code) => code,
            Err(error) => {
                self.issue(&error.message);
                return Ok(());
            }
        };
        if code != "idle.editor.input-attribution" {
            return Ok(());
        }
        if note.version != 1
            || note.category != NoteKind::Label
            || !matches!(self.file.action, FileAction::Change | FileAction::Create)
            || operation.session != self.operation.session
            || !self
                .file
                .revision
                .is_some_and(|revision| note.items.contains(&revision))
        {
            self.issue("An input receipt has an unsupported contract or revision association.");
            return Ok(());
        }
        let payload = match records::bytes(
            self.queries,
            operation.id,
            ContentField::Record(Field::Content),
        ) {
            Ok(bytes) => bytes,
            Err(error) => {
                self.issue(&error.message);
                return Ok(());
            }
        };
        if !serde_json::from_slice::<EditorEventKind>(&payload)
            .is_ok_and(|event| valid_receipt(&event, self.operation.sequence, operation.sequence))
        {
            self.unmapped(
                entry,
                "An input receipt is incomplete or has an unsupported payload.",
            );
            return Ok(());
        }
        let mut sources = vec![records::source(self.entry), records::source(entry)];
        let label = if let Some(author) = operation.author {
            let (_, label, registrations) = self.author(author)?;
            sources.extend(registrations);
            format!("Human input recorded (local, unsigned). {label}")
        } else {
            "Human input recorded; person identity is unavailable".to_owned()
        };
        self.add(IndicatorKind::Human, &label, ranges, &sources);
        Ok(())
    }

    fn exposures(&mut self) -> Result<(), Failure> {
        let Some(revision) = self.file.revision else {
            return Ok(());
        };
        let text = self.result.text.clone();
        let map = ranges::TextMap::new(&text);
        for entry in self.scan(IndexKey::File(self.file.path))? {
            let Some(operation) = Operation::view(&entry.operation) else {
                continue;
            };
            let Kind::File(file) = &operation.kind else {
                continue;
            };
            if file.revision != Some(revision)
                || !matches!(file.action, FileAction::View | FileAction::Read)
            {
                continue;
            }
            if file.after != self.file.after {
                self.unmapped(
                    &entry,
                    "An exposure uses the selected revision identity with different content.",
                );
                continue;
            }
            if file.name != self.file.name {
                let selected = records::bytes(
                    self.queries,
                    self.operation.id,
                    ContentField::Record(Field::Path),
                );
                let exposed = records::bytes(
                    self.queries,
                    operation.id,
                    ContentField::Record(Field::Path),
                );
                if !matches!((selected, exposed), (Ok(left), Ok(right)) if left == right) {
                    self.unmapped(
                        &entry,
                        "An exposure path is different or unavailable for the selected revision.",
                    );
                    continue;
                }
            }
            match ranges::exposure(file, &map) {
                Ok(ranges) => {
                    let kind = if file.action == FileAction::Read {
                        IndicatorKind::Read
                    } else {
                        IndicatorKind::Exposure
                    };
                    let label = if kind == IndicatorKind::Read {
                        "Read interval recorded"
                    } else {
                        "Visibility recorded"
                    };
                    let label = file.duration_ms.map_or_else(
                        || format!("{label}; duration not recorded"),
                        |duration| format!("{label}: {duration} ms"),
                    );
                    if ranges.is_empty() {
                        self.issue("An exposure has no recorded ranges; coverage is unknown.");
                    }
                    self.add(kind, &label, &ranges, &[records::source(&entry)]);
                }
                Err(error) => self.unmapped(&entry, &error.message),
            }
        }
        Ok(())
    }

    fn issue(&mut self, message: &str) {
        if !self.result.issues.iter().any(|issue| issue == message) {
            self.result.issues.push(message.to_owned());
        }
    }

    fn unmapped(&mut self, entry: &HistoryEntry, message: &str) {
        self.issue(message);
        self.add(
            IndicatorKind::Unknown,
            message,
            &[],
            &[records::source(entry)],
        );
    }

    fn add(
        &mut self,
        kind: IndicatorKind,
        label: &str,
        ranges: &[TextRange],
        sources: &[SourceRecord],
    ) {
        let ranges: Vec<_> = if ranges.is_empty() {
            vec![None]
        } else {
            ranges.iter().cloned().map(Some).collect()
        };
        for range in ranges {
            if self.result.indicators.len() >= MAX_INDICATORS {
                self.issue("The decoration limit was reached; additional observations may exist.");
                break;
            }
            self.result.indicators.push(Indicator {
                kind,
                range,
                label: label.to_owned(),
                sources: sources.to_owned(),
            });
        }
    }
}

fn display(value: &str) -> String {
    value
        .chars()
        .filter(|character| !character.is_control())
        .take(160)
        .collect()
}

fn valid_receipt(event: &EditorEventKind, change: Option<u64>, receipt: Option<u64>) -> bool {
    let Some((change, receipt)) = change.zip(receipt) else {
        return false;
    };
    if change == 0 || change >= receipt {
        return false;
    }
    if let EditorEventKind::HumanEdit {
        change: target,
        signal,
    } = event
    {
        return *target == change
            && matches!(
                signal.as_str(),
                "editor_input" | "keyboard_selection" | "typing_correction" | "undo" | "redo"
            );
    }
    if let EditorEventKind::HumanEditBatch { edits, group } = event {
        let mut previous = 0;
        return !edits.is_empty()
            && edits.len() <= 1024
            && group.is_none_or(|group| {
                group > 0 && edits.first().is_some_and(|edit| group <= edit.change)
            })
            && edits.iter().any(|edit| edit.change == change)
            && edits.iter().all(|edit| {
                let valid = edit.change > previous
                    && edit.change < receipt
                    && matches!(
                        edit.signal.as_str(),
                        "editor_input" | "keyboard_selection" | "typing_correction"
                    );
                previous = edit.change;
                valid
            });
    }
    false
}
