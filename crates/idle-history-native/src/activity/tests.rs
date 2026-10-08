use std::io::{Cursor, Read as _};

use editchain_engine::{
    ContentId, Engine, FileEdit, OpId, PathId, Payload,
    activity::{
        Author, AuthorRole, ChangeState, File, FileAction, ItemId, Kind, Note, NoteKind, Operation,
        TextEdit, TextRange,
    },
    queries::{ChainQueries, IndexKey, PageRequest},
};
use idle_history::binding::RepositoryChainBinding;
use serde_json::{Value, json};

use crate::history::{self, FailureCode, Source, Target, reference, service};

use super::{IndicatorKind, Preview, Request, Selection, prepare};

const SESSION: &str = "12345678-1111-4111-8111-123456789abc";

fn id(byte: u8) -> OpId {
    OpId::from_bytes([byte; 32])
}

fn binding() -> RepositoryChainBinding {
    RepositoryChainBinding {
        workspace_id: "workspace".into(),
        repository_id: "repo".into(),
        chain: "chain".into(),
    }
}

fn file(byte: u8, after: Option<ContentId>) -> Operation {
    Operation::new(
        id(byte),
        ItemId(id(byte)),
        ItemId(id(250)),
        Kind::File(File {
            action: FileAction::Change,
            path: PathId(1),
            name: Payload::Inline(b"a.rs".to_vec()),
            renamed_to: None,
            revision: Some(ItemId(id(byte))),
            before: None,
            after,
            edit: FileEdit::None,
            text_edits: Vec::new(),
            change: Some(ChangeState::Applied),
            caused_by: None,
            ranges: Vec::new(),
            text_ranges: Vec::new(),
            duration_ms: None,
        }),
    )
}

fn edit_file(operation: &mut Operation) -> Option<&mut File> {
    if let Kind::File(file) = &mut operation.kind {
        Some(file)
    } else {
        None
    }
}

fn append(engine: &Engine, operation: Operation) {
    let _admission = engine.append(&operation.into_op().unwrap()).unwrap();
}

fn author(byte: u8, item: u8, role: AuthorRole) -> Operation {
    Operation::new(
        id(byte),
        ItemId(id(item)),
        ItemId(id(250)),
        Kind::Author(Author {
            label: Payload::Inline(b"Recorded contributor".to_vec()),
            role,
            native_role: Payload::Empty,
            metadata: Payload::Empty,
        }),
    )
}

fn request(queries: &ChainQueries, byte: u8) -> Request {
    Request {
        binding: binding(),
        source: Source::Current,
        selection: Selection::Record(reference(
            queries
                .record_variants(id(byte))
                .unwrap()
                .first()
                .unwrap()
                .reference,
        )),
    }
}

fn run(queries: &mut ChainQueries, request: &Request) -> Preview {
    prepare(queries, &binding(), request.source, request).unwrap()
}

fn event(sequence: u64, event: Value) -> Value {
    let mut envelope = json!({"schema":1, "session":SESSION, "sequence":sequence, "time_ms":sequence.saturating_mul(1000),
        "identity":{"kind":"unsigned","guid":"abcd1234-1234-4234-8234-123456789abc","stream":"abcdefabcdefabcdefabcdef"},
        "event":null});
    *envelope.get_mut("event").unwrap() = event;
    envelope
}

fn document(version: u64) -> Value {
    json!({"id":"buffer","uri":"file:///checkout/a.rs","path":"a.rs","version":version})
}

#[test]
fn capture_receipts_exposures_and_gaps_refresh_without_merging_equal_buffers() {
    let root = tempfile::tempdir().unwrap();
    let mut writer = idle_editor_capture::CaptureWriter::default();
    let mut capture = |events: Vec<Value>| {
        let raw = serde_json::to_vec(
            &json!({"workspace_path":root.path(),"chain_dir":"chain","events":events}),
        )
        .unwrap();
        let _result = writer.record_json(&raw).unwrap();
    };
    capture(vec![
        event(
            1,
            json!({"type":"tracking_started","dwell_ms":500,"vscode_version":"1.85.0","activity_schema":3}),
        ),
        event(
            2,
            json!({"type":"document_snapshot","document":document(1),"text":"a😀\r\nz"}),
        ),
        event(
            3,
            json!({"type":"document_changed","document":document(2),"before_version":1,
            "before":"a😀\r\nz","after":"aé\r\nz","reason":null,"changes":[{"offset":1,"length":2,"text":"é"}]}),
        ),
    ]);
    let mut queries = ChainQueries::open(&root.path().join("chain")).unwrap();
    let selected = Request {
        binding: binding(),
        source: Source::Current,
        selection: Selection::Editor {
            session: SESSION.into(),
            document: "buffer".into(),
            version: 2,
        },
    };
    let pending = run(&mut queries, &selected);
    assert!(
        !pending
            .indicators
            .iter()
            .any(|indicator| indicator.kind == IndicatorKind::Human),
        "observed edits alone cannot establish human input"
    );
    capture(vec![
        event(
            4,
            json!({"type":"human_edit","change":3,"signal":"keyboard_selection"}),
        ),
        event(
            5,
            json!({"type":"code_read","document":document(2),"editor":"one","ranges":[{"start":[0,1],"end":[0,2]}],"started_ms":4000,"duration_ms":500}),
        ),
        event(
            6,
            json!({"type":"tracking_gap","reason":"capture interrupted"}),
        ),
    ]);
    let current = run(&mut queries, &selected);
    let expected = Some(TextRange {
        start: [0, 1],
        end: [0, 2],
    });
    let human = current
        .indicators
        .iter()
        .find(|indicator| indicator.kind == IndicatorKind::Human)
        .unwrap();
    assert_eq!(
        human.range, expected,
        "UTF-16 replacements map onto resulting Unicode content"
    );
    assert!(
        current
            .indicators
            .iter()
            .any(|indicator| indicator.kind == IndicatorKind::Read && indicator.range == expected),
        "only the recorded exposure interval is marked"
    );
    assert!(
        current
            .issues
            .iter()
            .any(|issue| issue.contains("capture gap")),
        "a gap cannot be hidden by positive activity"
    );
    for source in &human.sources {
        let opened = history::prepare(
            &mut queries,
            &binding(),
            Source::Current,
            &history::Request {
                binding: binding(),
                source: Source::Current,
                record: source.record.clone(),
                target: Target::Original,
            },
        )
        .unwrap();
        assert_eq!(
            opened.documents.len(),
            1,
            "every capture source drills down through native history"
        );
    }
    capture(vec![event(
        7,
        json!({"type":"document_snapshot","document":document(3),"text":"aé\r\nz"}),
    )]);
    let mut another = selected.clone();
    another.selection = Selection::Editor {
        session: SESSION.into(),
        document: "buffer".into(),
        version: 3,
    };
    let repeated = run(&mut queries, &another);
    assert_eq!(
        repeated.text, current.text,
        "fixture repeats identical bytes"
    );
    assert_ne!(
        repeated.revision, current.revision,
        "occurrence identities remain distinct"
    );
    assert!(
        !repeated
            .indicators
            .iter()
            .any(|indicator| matches!(indicator.kind, IndicatorKind::Human | IndicatorKind::Read)),
        "activity from an equal earlier buffer cannot leak"
    );
}

#[test]
fn agent_author_is_separate_from_recorder_and_late_conflicts_retract_exposure() {
    let root = tempfile::tempdir().unwrap();
    let engine = Engine::open(root.path()).unwrap();
    let content = engine.store_blob(b"code").unwrap().id;
    let mut revision = file(100, Some(content));
    revision.author = Some(ItemId(id(240)));
    append(&engine, revision);
    // An agent recorder says nothing about the author of a different observation.
    append(&engine, author(110, 250, AuthorRole::Agent));
    append(&engine, file(101, Some(content)));
    let mut queries = engine.queries().unwrap();
    let selected = request(&queries, 100);
    let unknown = run(&mut queries, &selected);
    assert!(
        !unknown
            .indicators
            .iter()
            .any(|indicator| indicator.kind == IndicatorKind::Ai),
        "missing author registration stays unknown"
    );
    append(&engine, author(1, 240, AuthorRole::Agent));
    let mut exposure = file(2, Some(content));
    let payload = edit_file(&mut exposure).unwrap();
    payload.action = FileAction::Read;
    payload.change = None;
    payload.revision = Some(ItemId(id(100)));
    payload.text_ranges = vec![TextRange {
        start: [0, 0],
        end: [0, 4],
    }];
    payload.duration_ms = Some(500);
    append(&engine, exposure.clone());
    let known = run(&mut queries, &selected);
    assert!(
        known
            .indicators
            .iter()
            .any(|indicator| indicator.kind == IndicatorKind::Ai),
        "lower-ID author arrivals refresh classification"
    );
    assert!(
        known
            .indicators
            .iter()
            .any(|indicator| indicator.kind == IndicatorKind::Read),
        "lower-ID exposure arrivals refresh coverage"
    );
    let recorder_only = request(&queries, 101);
    assert!(
        !run(&mut queries, &recorder_only)
            .indicators
            .iter()
            .any(|indicator| indicator.kind == IndicatorKind::Ai),
        "recorder role is never author role"
    );
    edit_file(&mut exposure).unwrap().duration_ms = Some(600);
    append(&engine, exposure);
    let conflicted = run(&mut queries, &selected);
    assert!(
        !conflicted
            .indicators
            .iter()
            .any(|indicator| indicator.kind == IndicatorKind::Read),
        "quarantined exposure is retracted"
    );
    assert!(
        conflicted
            .issues
            .iter()
            .any(|issue| issue.contains("quarantined")),
        "conflict limitations remain visible"
    );
    append(&engine, author(3, 240, AuthorRole::Person));
    assert!(
        !run(&mut queries, &selected)
            .indicators
            .iter()
            .any(|indicator| matches!(indicator.kind, IndicatorKind::Human | IndicatorKind::Ai)),
        "inconsistent author registrations never select a latest role"
    );
}

#[test]
fn missing_empty_late_proposed_and_conflicted_snapshots_remain_distinct() {
    let root = tempfile::tempdir().unwrap();
    let other = tempfile::tempdir().unwrap();
    let engine = Engine::open(root.path()).unwrap();
    let late = Engine::open(other.path())
        .unwrap()
        .store_blob(b"late")
        .unwrap()
        .id;
    append(&engine, file(10, Some(late)));
    append(&engine, file(11, None));
    let empty = engine.store_blob(b"").unwrap().id;
    append(&engine, file(12, Some(empty)));
    let mut proposed = file(13, Some(empty));
    edit_file(&mut proposed).unwrap().change = Some(ChangeState::Proposed);
    append(&engine, proposed);
    let mut queries = engine.queries().unwrap();
    for (byte, code) in [
        (10, FailureCode::MissingContent),
        (11, FailureCode::NotRecorded),
        (13, FailureCode::Unavailable),
    ] {
        let selected = request(&queries, byte);
        assert_eq!(
            prepare(&mut queries, &binding(), Source::Current, &selected)
                .unwrap_err()
                .code,
            code,
            "snapshot unavailability has an explicit reason"
        );
    }
    let selected = request(&queries, 12);
    assert_eq!(
        run(&mut queries, &selected).text,
        "",
        "a recorded empty buffer remains available"
    );
    let _blob = engine.store_blob(b"late").unwrap();
    let selected = request(&queries, 10);
    assert_eq!(
        run(&mut queries, &selected).text,
        "late",
        "late content is observed on refresh"
    );
    let mut wrong = selected.clone();
    wrong.binding.repository_id = "different-checkout".into();
    assert_eq!(
        prepare(&mut queries, &binding(), Source::Current, &wrong)
            .unwrap_err()
            .code,
        FailureCode::BindingMismatch,
        "repository bindings cannot cross"
    );
    append(&engine, file(10, Some(empty)));
    assert_eq!(
        prepare(&mut queries, &binding(), Source::Current, &selected)
            .unwrap_err()
            .code,
        FailureCode::Conflicted,
        "conflicting snapshots never supply ranges"
    );
}

#[test]
fn exact_multi_edit_mapping_preserves_surrogates_and_separate_insertions() {
    let root = tempfile::tempdir().unwrap();
    let engine = Engine::open(root.path()).unwrap();
    let before = engine.store_blob("a😀\r\nz".as_bytes()).unwrap().id;
    let after = engine.store_blob("Ba😀\r\nzX".as_bytes()).unwrap().id;
    let mut operation = file(10, Some(after));
    operation.author = Some(ItemId(id(240)));
    let file = edit_file(&mut operation).unwrap();
    file.before = Some(before);
    file.text_edits = vec![
        TextEdit {
            offset_utf16: 6,
            length_utf16: 0,
            text: Payload::Inline(b"X".to_vec()),
        },
        TextEdit {
            offset_utf16: 0,
            length_utf16: 0,
            text: Payload::Inline(b"B".to_vec()),
        },
    ];
    append(&engine, operation);
    append(&engine, author(1, 240, AuthorRole::Agent));
    let mut queries = engine.queries().unwrap();
    let selected = request(&queries, 10);
    let result = run(&mut queries, &selected);
    let ranges: Vec<_> = result
        .indicators
        .iter()
        .filter(|indicator| indicator.kind == IndicatorKind::Ai)
        .map(|indicator| indicator.range.clone())
        .collect();
    assert_eq!(
        ranges,
        vec![
            Some(TextRange {
                start: [0, 0],
                end: [0, 1]
            }),
            Some(TextRange {
                start: [1, 1],
                end: [1, 2]
            })
        ],
        "unchanged text between independent edits is not attributed"
    );
}

#[test]
fn malformed_ranges_and_missing_revision_identity_cannot_claim_coverage() {
    let root = tempfile::tempdir().unwrap();
    let engine = Engine::open(root.path()).unwrap();
    let content = engine.store_blob("a😀\r\nz".as_bytes()).unwrap().id;
    append(&engine, file(10, Some(content)));
    let mut exposure = file(2, Some(content));
    let payload = edit_file(&mut exposure).unwrap();
    payload.action = FileAction::Read;
    payload.change = None;
    payload.revision = Some(ItemId(id(10)));
    payload.text_ranges = vec![TextRange {
        start: [0, 2],
        end: [0, 3],
    }];
    append(&engine, exposure);
    let mut queries = engine.queries().unwrap();
    let selected = request(&queries, 10);
    let result = run(&mut queries, &selected);
    assert!(
        !result
            .indicators
            .iter()
            .any(|indicator| indicator.kind == IndicatorKind::Read),
        "a range splitting a surrogate pair cannot be painted"
    );
    assert!(
        result.issues.iter().any(|issue| issue.contains("UTF-16")),
        "invalid coordinates are explained"
    );
    let mut no_revision = file(11, Some(content));
    edit_file(&mut no_revision).unwrap().revision = None;
    append(&engine, no_revision);
    let _delta = queries.refresh().unwrap();
    let selected = request(&queries, 11);
    assert!(
        run(&mut queries, &selected)
            .issues
            .iter()
            .any(|issue| issue.contains("No revision identity")),
        "content alone does not identify exposure coverage"
    );
}

#[test]
fn unsupported_receipts_keep_sources_and_read_observers_do_not_become_code_authors() {
    let root = tempfile::tempdir().unwrap();
    let engine = Engine::open(root.path()).unwrap();
    let content = engine.store_blob(b"a").unwrap().id;
    let mut revision = file(10, Some(content));
    revision.sequence = Some(3);
    revision.session = Some(ItemId(id(200)));
    append(&engine, revision);
    for (byte, change, signal) in [(1, 2, "editor_input"), (2, 3, "unsupported_input")] {
        let mut receipt = Operation::new(
            id(byte),
            ItemId(id(byte)),
            ItemId(id(250)),
            Kind::Note(Note {
                category: NoteKind::Label,
                targets: vec![id(10)],
                items: vec![ItemId(id(10))],
                version: 1,
                code: Payload::Inline(b"idle.editor.input-attribution".to_vec()),
                content: Payload::Inline(
                    serde_json::to_vec(
                        &json!({"type":"human_edit","change":change,"signal":signal}),
                    )
                    .unwrap(),
                ),
            }),
        );
        receipt.sequence = Some(4);
        receipt.session = Some(ItemId(id(200)));
        append(&engine, receipt);
    }
    let mut read = file(5, Some(content));
    read.author = Some(ItemId(id(240)));
    let payload = edit_file(&mut read).unwrap();
    payload.action = FileAction::Read;
    payload.change = None;
    payload.revision = Some(ItemId(id(10)));
    payload.text_ranges = vec![TextRange {
        start: [0, 0],
        end: [0, 1],
    }];
    payload.name = Payload::Blob(engine.store_blob(b"a.rs").unwrap());
    append(&engine, read);
    append(&engine, author(6, 240, AuthorRole::Person));
    let mut queries = engine.queries().unwrap();
    let selected = request(&queries, 10);
    let result = run(&mut queries, &selected);
    assert!(
        !result
            .indicators
            .iter()
            .any(|indicator| indicator.kind == IndicatorKind::Human),
        "wrong change sequences and unsupported input signals stay unclaimed"
    );
    for byte in [1, 2] {
        assert!(
            result.indicators.iter().any(|indicator| indicator
                .sources
                .iter()
                .any(|source| source.record.operation == id(byte).to_string())),
            "incomplete receipts still provide exact source drill-down"
        );
    }
    assert!(
        result
            .indicators
            .iter()
            .any(|indicator| indicator.kind == IndicatorKind::Read),
        "inline and blob path encodings resolve to the same recorded path"
    );
    let selected = request(&queries, 5);
    assert!(
        !run(&mut queries, &selected)
            .indicators
            .iter()
            .any(|indicator| indicator.kind == IndicatorKind::Human),
        "the observer of a read does not become the author of the code"
    );
}

#[test]
fn limited_note_queries_report_incomplete_observations() {
    let root = tempfile::tempdir().unwrap();
    let engine = Engine::open(root.path()).unwrap();
    let content = engine.store_blob(b"a").unwrap().id;
    append(&engine, file(1, Some(content)));
    {
        let mut writer = engine.writer().unwrap();
        for sequence in 0_u32..4001 {
            let operation = ItemId::derive("test.notes", &sequence.to_le_bytes()).0;
            let note = Operation::new(
                operation,
                ItemId(operation),
                ItemId(id(250)),
                Kind::Note(Note {
                    category: NoteKind::Comment,
                    targets: Vec::new(),
                    items: Vec::new(),
                    version: 1,
                    code: Payload::Empty,
                    content: Payload::Empty,
                }),
            )
            .into_op()
            .unwrap();
            let _admission = writer.append(&note).unwrap();
        }
    }
    let mut queries = engine.queries().unwrap();
    let selected = request(&queries, 1);
    assert!(
        run(&mut queries, &selected)
            .issues
            .iter()
            .any(|issue| issue.contains("query limit")),
        "a bounded result never implies complete observations"
    );
}

#[test]
fn framed_activity_reads_reuse_bound_history_sources() {
    let root = tempfile::tempdir().unwrap();
    let engine = Engine::open(root.path()).unwrap();
    let content = engine.store_blob(b"recorded").unwrap().id;
    append(&engine, file(1, Some(content)));
    let mut selected = request(&engine.queries().unwrap(), 1);
    let mut input = Vec::new();
    for source in [Source::Current, Source::Retained] {
        selected.source = source;
        let body = serde_json::to_vec(&json!({"id":1,"body":{"activity":selected}})).unwrap();
        input.extend_from_slice(&u32::try_from(body.len()).unwrap().to_le_bytes());
        input.extend_from_slice(&body);
    }
    let mut output = Vec::new();
    service::serve(
        Cursor::new(input),
        &mut output,
        &service::Binding {
            repository: binding(),
            chain_directory: root.path().to_path_buf(),
            retained_directory: None,
            repository_directory: None,
        },
    )
    .unwrap();
    let mut output = Cursor::new(output);
    let mut read = || {
        let mut header = [0; 4];
        output.read_exact(&mut header).unwrap();
        let mut bytes = vec![0; usize::try_from(u32::from_le_bytes(header)).unwrap()];
        output.read_exact(&mut bytes).unwrap();
        serde_json::from_slice::<Value>(&bytes).unwrap()
    };
    assert_eq!(
        read().pointer("/body/Ok/text"),
        Some(&json!("recorded")),
        "packaged RPC returns the verified snapshot"
    );
    assert_eq!(
        read().pointer("/body/Err/code"),
        Some(&json!("unavailable")),
        "retained sources require a separate binding"
    );
    let page = engine
        .queries()
        .unwrap()
        .history(Some(IndexKey::File(PathId(1))), PageRequest::default())
        .unwrap();
    assert_eq!(
        page.items.len(),
        1,
        "activity reads never append synthetic observations"
    );
}
