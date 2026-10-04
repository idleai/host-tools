use std::{
    error::Error,
    io::{Cursor, Read as _},
};

use editchain_engine::{
    ActorId, Clock, ContentId, Engine, FileEdit, MessageOp, Op, OpId, OpKind, ParentSet, PathId,
    Payload, ScopeRef, Tags,
    activity::{File, FileAction, ItemId, Kind, Operation, Original, OriginalRef},
    queries::{ChainQueries, ContentField},
};
use idle_history::binding::RepositoryChainBinding;

use super::{FailureCode, Preview, Request, Source, Target, prepare, reference, service};

type Result<T = ()> = std::result::Result<T, Box<dyn Error>>;

fn id(byte: u8) -> OpId {
    OpId::from_bytes([byte; 32])
}

fn binding() -> RepositoryChainBinding {
    RepositoryChainBinding {
        workspace_id: "workspace".into(),
        repository_id: "repository".into(),
        chain: "chain".into(),
    }
}

fn activity(byte: u8, kind: Kind) -> Operation {
    Operation::new(id(byte), ItemId(id(byte)), ItemId(id(250)), kind)
}

fn file(byte: u8, before: Option<ContentId>, after: Option<ContentId>) -> Result<Op> {
    Ok(activity(
        byte,
        Kind::File(File {
            action: FileAction::Change,
            path: PathId(1),
            name: Payload::Inline(b"src/exact.rs".to_vec()),
            renamed_to: None,
            revision: Some(ItemId(id(byte))),
            before,
            after,
            edit: FileEdit::None,
            text_edits: Vec::new(),
            change: None,
            caused_by: None,
            ranges: Vec::new(),
            text_ranges: Vec::new(),
            duration_ms: None,
        }),
    )
    .into_op()?)
}

fn original(byte: u8, bytes: Payload) -> Result<Op> {
    Ok(activity(
        byte,
        Kind::Original(Original {
            provider: "test".into(),
            format: None,
            native: Vec::new(),
            location: None,
            bytes,
            hash: None,
        }),
    )
    .into_op()?)
}

fn request(queries: &ChainQueries, operation: OpId, target: Target) -> Result<Request> {
    let records = queries.record_variants(operation)?;
    let record = records.first().ok_or("fixture record missing")?;
    Ok(Request {
        binding: binding(),
        source: Source::Current,
        record: reference(record.reference),
        target,
    })
}

fn run(
    queries: &mut ChainQueries,
    request: &Request,
) -> std::result::Result<Preview, super::Failure> {
    prepare(queries, &binding(), Source::Current, request)
}

#[test]
fn complete_file_diff_and_record_preserve_every_byte() {
    let directory = tempfile::tempdir().unwrap();
    let engine = Engine::open(directory.path()).unwrap();
    let before = b"\xef\xbb\xbflet s = \"\xf0\x9f\x98\x80\";\r\n\n";
    let after = b"\x00\xff\xfe\n\r\n";
    let operation = file(
        1,
        Some(engine.store_blob(before).unwrap().id),
        Some(engine.store_blob(after).unwrap().id),
    )
    .unwrap();
    let expected_record = editchain_engine::encode_op(&operation).unwrap();
    let _admitted = engine.append_encoded(&expected_record).unwrap();
    let mut queries = engine.queries().unwrap();
    let mut selected = request(&queries, id(1), Target::Diff).unwrap();
    let preview = run(&mut queries, &selected).unwrap();
    let [left, right]: &[super::Document; 2] = preview
        .documents
        .as_slice()
        .try_into()
        .expect("both diff sides");
    assert_eq!(
        left.bytes, before,
        "before bytes include BOM, Unicode and mixed line endings"
    );
    assert_eq!(
        right.bytes, after,
        "binary after bytes must not be decoded as text"
    );
    let roundtrip: Preview =
        serde_json::from_slice(&serde_json::to_vec(&preview).unwrap()).unwrap();
    assert_eq!(roundtrip, preview, "the shared transport codec is lossless");
    selected.target = Target::File;
    assert_eq!(
        run(&mut queries, &selected)
            .unwrap()
            .documents
            .first()
            .ok_or("file")
            .unwrap()
            .bytes,
        after,
        "file preview uses the recorded after snapshot"
    );
    selected.target = Target::Record;
    assert_eq!(
        run(&mut queries, &selected)
            .unwrap()
            .documents
            .first()
            .ok_or("record")
            .unwrap()
            .bytes,
        expected_record,
        "raw view uses the retained encoding"
    );
}

#[test]
fn missing_empty_absent_late_corrupt_and_local_content_stay_distinct() {
    let directory = tempfile::tempdir().unwrap();
    let other = tempfile::tempdir().unwrap();
    let engine = Engine::open(directory.path()).unwrap();
    let late = b"late\r\n";
    let address = Engine::open(other.path())
        .unwrap()
        .store_blob(late)
        .unwrap()
        .id;
    let empty = engine.store_blob(b"").unwrap().id;
    let _stored = engine
        .append(&file(1, Some(empty), Some(address)).unwrap())
        .unwrap();
    let _stored = engine.append(&file(2, None, Some(empty)).unwrap()).unwrap();
    let _stored = engine
        .append(
            &file(
                3,
                Some(empty),
                Some(ContentId::Local {
                    node: editchain_engine::NodeId(1),
                    seq: 12,
                }),
            )
            .unwrap(),
        )
        .unwrap();
    let mut queries = engine.queries().unwrap();
    let selected = request(&queries, id(1), Target::Diff).unwrap();
    assert_eq!(
        run(&mut queries, &selected)
            .expect_err("blob is missing")
            .code,
        FailureCode::MissingContent,
        "no blank diff side for missing bytes"
    );
    let empty_file = request(&queries, id(2), Target::File).unwrap();
    assert!(
        run(&mut queries, &empty_file)
            .unwrap()
            .documents
            .first()
            .ok_or("empty file")
            .unwrap()
            .bytes
            .is_empty(),
        "a verified empty file opens"
    );
    let absent_base = request(&queries, id(2), Target::Diff).unwrap();
    assert_eq!(
        run(&mut queries, &absent_base)
            .expect_err("base absent")
            .code,
        FailureCode::NotRecorded,
        "missing base is not inferred from file action"
    );
    let local = request(&queries, id(3), Target::File).unwrap();
    assert_eq!(
        run(&mut queries, &local).expect_err("local content").code,
        FailureCode::UnresolvableContent,
        "local IDs are not global content hashes"
    );
    let _blob = engine.store_blob(late).unwrap();
    assert_eq!(
        run(&mut queries, &selected)
            .unwrap()
            .documents
            .last()
            .ok_or("late bytes")
            .unwrap()
            .bytes,
        late,
        "refresh observes a late blob"
    );
    let hash = match address {
        ContentId::Hash256(hash) => Some(hash),
        ContentId::Local { .. } | ContentId::Hash128(_) => None,
    }
    .expect("full address");
    std::fs::write(
        directory
            .path()
            .join("blobs")
            .join(OpId::from_bytes(hash).to_string()),
        b"damage",
    )
    .unwrap();
    assert_eq!(
        run(&mut queries, &selected).expect_err("corruption").code,
        FailureCode::CorruptContent,
        "damaged content cannot become a preview"
    );
}

#[test]
fn exact_original_reference_and_content_length_are_verified() {
    let directory = tempfile::tempdir().unwrap();
    let engine = Engine::open(directory.path()).unwrap();
    let bytes = b" { \"text\": \"raw\" } \r\n";
    let blob = engine.store_blob(bytes).unwrap();
    let _stored = engine
        .append(&original(1, Payload::Blob(blob)).unwrap())
        .unwrap();
    let mut derived = activity(
        2,
        Kind::Original(Original {
            provider: "test".into(),
            format: None,
            native: Vec::new(),
            location: None,
            bytes: Payload::Inline(Vec::new()),
            hash: None,
        }),
    );
    // Use a file observation to ensure the action follows its explicit Original.
    let file_op = file(2, None, None).unwrap();
    derived.kind = Operation::view(&file_op)
        .ok_or("file adapter")
        .unwrap()
        .kind;
    derived.original = Some(OriginalRef {
        operation: id(1),
        converter: "test-v1".into(),
    });
    let _stored = engine.append(&derived.into_op().unwrap()).unwrap();
    let mut queries = engine.queries().unwrap();
    let selected = request(&queries, id(2), Target::Original).unwrap();
    let preview = run(&mut queries, &selected).unwrap();
    let document = preview.documents.first().ok_or("Original").unwrap();
    assert_eq!(
        document.bytes, bytes,
        "source whitespace and trailing newline are exact"
    );
    assert_eq!(
        document.record.operation,
        id(1).to_string(),
        "the document retains its actual source record"
    );
    let field = ContentField::Record(editchain_engine::activity::Field::Content);
    let mut content = request(
        &queries,
        id(1),
        Target::Content {
            field,
            reference: Some(blob.into()),
        },
    )
    .unwrap();
    assert_eq!(
        run(&mut queries, &content)
            .unwrap()
            .documents
            .first()
            .ok_or("content")
            .unwrap()
            .bytes,
        bytes,
        "field resolves its complete blob reference"
    );
    content.target = Target::Content {
        field,
        reference: Some(editchain_engine::queries::ContentReference {
            id: blob.id,
            len: Some(blob.len.saturating_add(1)),
        }),
    };
    assert_eq!(
        run(&mut queries, &content).expect_err("wrong length").code,
        FailureCode::InvalidReference,
        "a hash alone cannot replace a declared length"
    );
}

#[test]
fn conflicts_allow_only_exact_raw_variants_and_never_choose_content() {
    let directory = tempfile::tempdir().unwrap();
    let engine = Engine::open(directory.path()).unwrap();
    let first = original(1, Payload::Inline(b"one".to_vec())).unwrap();
    let second = original(1, Payload::Inline(b"two".to_vec())).unwrap();
    let _stored = engine.append(&first).unwrap();
    let mut queries = engine.queries().unwrap();
    let mut selected = request(&queries, id(1), Target::Original).unwrap();
    let _stored = engine.append(&second).unwrap();
    let failure = run(&mut queries, &selected).expect_err("conflict");
    assert_eq!(
        failure.code,
        FailureCode::Conflicted,
        "late conflict retracts content actions"
    );
    assert_eq!(
        failure.candidates.len(),
        2,
        "every representation stays inspectable"
    );
    selected.target = Target::Record;
    for candidate in failure.candidates {
        selected.record = candidate;
        let preview = run(&mut queries, &selected).unwrap();
        let bytes = &preview.documents.first().ok_or("variant").unwrap().bytes;
        assert!(
            bytes == &editchain_engine::encode_op(&first).unwrap()
                || bytes == &editchain_engine::encode_op(&second).unwrap(),
            "only a retained variant can open"
        );
    }
    selected.record.hash = id(99).to_string();
    assert_eq!(
        run(&mut queries, &selected)
            .expect_err("digest mismatch")
            .code,
        FailureCode::RecordMismatch,
        "full digest is checked even for raw records"
    );
}

#[test]
fn actual_schema_migration_requires_explicit_alias_or_retained_selection() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("source");
    let destination = directory.path().join("converted");
    let engine = Engine::open(&source).unwrap();
    let legacy = Op {
        id: id(1),
        source: None,
        parents: ParentSet::None,
        actor: ActorId(1),
        clock: Clock::None,
        scope: ScopeRef::None,
        tags: Tags::NONE,
        kind: OpKind::Message(MessageOp {
            content: Payload::Inline(b"legacy\r\n".to_vec()),
            content_type: Payload::Empty,
        }),
    };
    let _stored = engine.append(&legacy).unwrap();
    let mut selected = request(&engine.queries().unwrap(), id(1), Target::Record).unwrap();
    let _report = idle_history_import::activity::migrate(&source, &destination, || false).unwrap();
    let mut converted = ChainQueries::open(&destination).unwrap();
    let alias = run(&mut converted, &selected).expect_err("old reference");
    assert_eq!(
        alias.code,
        FailureCode::MigratedAlias,
        "converted bytes cannot satisfy the old reference"
    );
    assert_eq!(
        alias.candidates.len(),
        1,
        "the new representation is offered explicitly"
    );
    let original_record = selected.record.clone();
    selected.record = alias
        .candidates
        .first()
        .ok_or("converted reference")
        .unwrap()
        .clone();
    let new_bytes = run(&mut converted, &selected)
        .unwrap()
        .documents
        .first()
        .ok_or("converted bytes")
        .unwrap()
        .bytes
        .clone();
    assert_ne!(
        new_bytes,
        editchain_engine::encode_op(&legacy).unwrap(),
        "schema conversion changes the stored encoding"
    );
    selected.record = original_record;
    selected.source = Source::Retained;
    assert_eq!(
        run(&mut converted, &selected)
            .expect_err("wrong source")
            .code,
        FailureCode::BindingMismatch,
        "retained selection cannot run on the current source"
    );
    let mut retained = ChainQueries::open(&destination.join("migration-v1/original")).unwrap();
    let raw = prepare(&mut retained, &binding(), Source::Retained, &selected).unwrap();
    assert_eq!(
        raw.documents
            .first()
            .ok_or("retained record")
            .unwrap()
            .bytes,
        editchain_engine::encode_op(&legacy).unwrap(),
        "retained segments preserve exact pre-conversion bytes"
    );
}

#[test]
fn ambiguous_aliases_and_retained_physical_ids_are_explicit() {
    let directory = tempfile::tempdir().unwrap();
    let engine = Engine::open(directory.path()).unwrap();
    let old = Op {
        id: id(1),
        source: None,
        parents: ParentSet::None,
        actor: ActorId(1),
        clock: Clock::None,
        scope: ScopeRef::None,
        tags: Tags::NONE,
        kind: OpKind::Message(MessageOp {
            content: Payload::Inline(b"old".to_vec()),
            content_type: Payload::Empty,
        }),
    };
    let upgraded = Operation::upgrade(&old).ok_or("upgrade").unwrap();
    let mut alternative = upgraded.clone();
    alternative.id = id(2);
    let _stored = engine.append(&upgraded.into_op().unwrap()).unwrap();
    let _stored = engine.append(&alternative.into_op().unwrap()).unwrap();
    let mut queries = engine.queries().unwrap();
    let mut selected = Request {
        binding: binding(),
        source: Source::Current,
        record: idle_history::query::RecordRef {
            operation: id(1).to_string(),
            hash: id(99).to_string(),
        },
        target: Target::Record,
    };
    let failure = run(&mut queries, &selected).expect_err("multiple aliases");
    assert_eq!(
        failure.code,
        FailureCode::MigratedAlias,
        "ambiguity requires explicit selection"
    );
    assert_eq!(
        failure.candidates.len(),
        2,
        "both converted representations are retained"
    );
    let _stored = engine.append(&old).unwrap();
    let _changes = queries.refresh().unwrap();
    selected = request(&queries, id(1), Target::Record).unwrap();
    assert_eq!(
        run(&mut queries, &selected)
            .unwrap()
            .documents
            .first()
            .ok_or("physical record")
            .unwrap()
            .bytes,
        editchain_engine::encode_op(&old).unwrap(),
        "a physically retained ID takes precedence over aliases"
    );
}

#[test]
fn bindings_and_full_references_are_required_before_reads() {
    let directory = tempfile::tempdir().unwrap();
    let engine = Engine::open(directory.path()).unwrap();
    let _stored = engine
        .append(&original(1, Payload::Inline(Vec::new())).unwrap())
        .unwrap();
    let mut queries = engine.queries().unwrap();
    let selected = request(&queries, id(1), Target::Original).unwrap();
    for field in ["operation", "hash"] {
        let mut incomplete = selected.clone();
        if field == "operation" {
            incomplete.record.operation = "0101".into();
        } else {
            incomplete.record.hash = "0101".into();
        }
        assert_eq!(
            run(&mut queries, &incomplete).expect_err("prefix").code,
            FailureCode::InvalidReference,
            "display prefixes cannot select documents"
        );
    }
    let mut foreign = selected.clone();
    foreign.binding.repository_id = "other".into();
    assert_eq!(
        run(&mut queries, &foreign).expect_err("repository").code,
        FailureCode::BindingMismatch,
        "shared chain does not imply the same checkout"
    );
    assert!(
        run(&mut queries, &selected)
            .unwrap()
            .documents
            .first()
            .ok_or("empty Original")
            .unwrap()
            .bytes
            .is_empty(),
        "recorded empty Original is available"
    );
}

#[test]
fn framed_service_checks_bound_sources_and_returns_exact_codec_bytes() {
    let directory = tempfile::tempdir().unwrap();
    let engine = Engine::open(directory.path()).unwrap();
    let bytes = vec![0, 255, 1, 13, 10];
    let _stored = engine
        .append(&original(1, Payload::Inline(bytes.clone())).unwrap())
        .unwrap();
    let selected = request(&engine.queries().unwrap(), id(1), Target::Original).unwrap();
    let mut unavailable = selected.clone();
    unavailable.source = Source::Retained;
    let mut input = Vec::new();
    for body in [
        serde_json::to_value(&selected).unwrap(),
        serde_json::json!({ "invalid": true }),
        serde_json::to_value(&unavailable).unwrap(),
    ] {
        let frame = serde_json::to_vec(&serde_json::json!({ "id": 1, "body": body })).unwrap();
        input.extend_from_slice(&u32::try_from(frame.len()).unwrap().to_le_bytes());
        input.extend_from_slice(&frame);
    }
    let mut output = Vec::new();
    service::serve(
        Cursor::new(input),
        &mut output,
        &service::Binding {
            repository: binding(),
            chain_directory: directory.path().to_path_buf(),
            retained_directory: None,
        },
    )
    .unwrap();
    let mut output = Cursor::new(output);
    let mut header = [0; 4];
    output.read_exact(&mut header).unwrap();
    let mut frame = vec![0; usize::try_from(u32::from_le_bytes(header)).unwrap()];
    output.read_exact(&mut frame).unwrap();
    let response: serde_json::Value = serde_json::from_slice(&frame).unwrap();
    let preview: Preview = serde_json::from_value(
        response
            .get("body")
            .and_then(|value| value.get("Ok"))
            .ok_or("response")
            .unwrap()
            .clone(),
    )
    .unwrap();
    assert_eq!(
        preview.documents.first().ok_or("document").unwrap().bytes,
        bytes,
        "framed binary payload survives unchanged"
    );
    output.read_exact(&mut header).unwrap();
    let mut frame = vec![0; usize::try_from(u32::from_le_bytes(header)).unwrap()];
    output.read_exact(&mut frame).unwrap();
    let response: serde_json::Value = serde_json::from_slice(&frame).unwrap();
    assert_eq!(
        response.pointer("/body/Err/code"),
        Some(&serde_json::json!("invalid_reference")),
        "malformed actions fail without closing the service"
    );
    output.read_exact(&mut header).unwrap();
    let mut frame = vec![0; usize::try_from(u32::from_le_bytes(header)).unwrap()];
    output.read_exact(&mut frame).unwrap();
    let response: serde_json::Value = serde_json::from_slice(&frame).unwrap();
    assert_eq!(
        response.pointer("/body/Err/code"),
        Some(&serde_json::json!("unavailable")),
        "retained source must be installed independently"
    );
}

#[test]
fn legacy_originals_and_unknown_payloads_use_exact_engine_fields() {
    let directory = tempfile::tempdir().unwrap();
    let engine = Engine::open(directory.path()).unwrap();
    let raw = b"\xff raw\r\n";
    let imported = Op {
        id: id(1),
        source: None,
        parents: ParentSet::None,
        actor: ActorId(1),
        clock: Clock::None,
        scope: ScopeRef::None,
        tags: Tags::NONE,
        kind: OpKind::Import(editchain_engine::ImportOp {
            raw_ref: Payload::Inline(raw.to_vec()),
            raw_hash: None,
        }),
    };
    let unknown = Op {
        id: id(2),
        kind: OpKind::Unknown(editchain_engine::UnknownOp {
            kind_discriminant: 254,
            raw_bytes: Payload::Inline(raw.to_vec()),
        }),
        ..imported.clone()
    };
    let _stored = engine.append(&imported).unwrap();
    let _stored = engine.append(&unknown).unwrap();
    let mut queries = engine.queries().unwrap();
    for (operation, field) in [
        (id(1), ContentField::ImportRaw),
        (id(2), ContentField::UnknownRaw),
    ] {
        let mut selected = request(&queries, operation, Target::Original).unwrap();
        let preview = run(&mut queries, &selected).unwrap();
        let document = preview.documents.first().expect("legacy Original");
        assert_eq!(
            document.bytes, raw,
            "legacy adapters cannot change the payload"
        );
        assert_eq!(
            document.field,
            Some(field),
            "keep the physical source field"
        );
        selected.target = Target::File;
        assert_eq!(
            run(&mut queries, &selected).expect_err("not a file").code,
            FailureCode::Unavailable,
            "wrong action has no invented snapshot"
        );
    }
}

#[test]
fn native_projection_reads_real_activity_and_rejects_other_bindings() {
    use idle_history::{binding as subscriptions, projections};

    let directory = tempfile::tempdir().unwrap();
    let engine = Engine::open(directory.path()).unwrap();
    let _stored = engine
        .append(&original(91, Payload::Inline(b"recorded".to_vec())).unwrap())
        .unwrap();
    drop(engine);
    let query = projections::ProjectionQuery {
        context: subscriptions::Context {
            provider: "idle-local".into(),
            workspace: "workspace".into(),
            contributor: "local-user".into(),
            chain: "chain".into(),
        },
        limit: 20,
        refresh_sources: false,
    };
    let installed = service::Binding {
        repository: binding(),
        chain_directory: directory.path().to_path_buf(),
        retained_directory: None,
    };
    let mut wrong = binding();
    wrong.repository_id = "another-repository".into();
    for selected in [binding(), wrong] {
        let bytes = serde_json::to_vec(
            &serde_json::json!({"id":1, "body":{"binding":selected,"projection":query}}),
        )
        .unwrap();
        let mut input = u32::try_from(bytes.len()).unwrap().to_le_bytes().to_vec();
        input.extend(bytes);
        let mut output = Vec::new();
        service::serve(Cursor::new(input), &mut output, &installed).unwrap();
        let response: serde_json::Value = serde_json::from_slice(output.get(4..).unwrap()).unwrap();
        let result: projections::ProjectionOutput =
            serde_json::from_value(response.get("body").unwrap().clone()).unwrap();
        if selected == binding() {
            let snapshot = result.unwrap();
            assert_eq!(snapshot.workspace_id, "workspace");
            assert!(
                snapshot
                    .inputs
                    .iter()
                    .any(|input| input.kind == projections::ProjectionKind::Activity
                        && !input.rows.is_empty())
            );
            assert!(
                snapshot
                    .inputs
                    .iter()
                    .filter(|input| input.kind != projections::ProjectionKind::Activity)
                    .all(|input| input.availability
                        == projections::ProjectionAvailability::Unavailable
                        && input.total.is_none())
            );
        } else {
            assert!(result.unwrap_err().message.contains("different repository"));
        }
    }
}
