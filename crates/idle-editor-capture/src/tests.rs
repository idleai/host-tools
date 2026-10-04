use crate::{CaptureWriter, identity};
use editchain_core::{
    ByteRange, FileEdit, OpKind, Payload,
    activity::{FileAction, Kind, NoteKind, Operation},
};
use editchain_store::{BlobReader, IndexedTail};
use serde_json::{Value, json};
use std::{collections::BTreeSet, fs, path::Path};

const SESSION: &str = "12345678-1111-4111-8111-123456789abc";

fn event(sequence: u64, body: Value) -> Value {
    let mut value = json!({"schema":1, "session":SESSION, "sequence":sequence, "time_ms":sequence.saturating_mul(1000),
        "identity":{"kind":"unsigned","guid":"abcd1234-1234-4234-8234-123456789abc","stream":"abcdefabcdefabcdefabcdef"},
        "units":{"offsets":"utf16_code_units","positions":"zero_based_line_utf16_column","snapshots":"utf8_bytes"},
        "event":null});
    let _old = value
        .as_object_mut()
        .expect("fixture object")
        .insert("event".into(), body);
    value
}

fn start() -> Value {
    event(
        1,
        json!({"type":"tracking_started", "dwell_ms":500, "vscode_version":"1.85.0", "activity_schema":3}),
    )
}

fn document(version: u64) -> Value {
    json!({"id":"buffer-1","uri":"file:///checkout/a.rs","path":"a.rs","version":version})
}

fn snapshot(sequence: u64) -> Value {
    event(
        sequence,
        json!({"type":"document_snapshot", "document":document(1), "text":"a😀\r\nz"}),
    )
}

fn change(sequence: u64) -> Value {
    event(
        sequence,
        json!({"type":"document_changed", "document":document(2), "before_version":1,
        "before":"a😀\r\nz", "after":"aé\r\nz", "reason":null,
        "changes":[{"offset":1,"length":2,"text":"é"}]}),
    )
}

fn batch(root: &Path, events: &[Value]) -> crate::Result<Vec<u8>> {
    Ok(serde_json::to_vec(
        &json!({"workspace_path": root, "chain_dir":".editchain", "events":events}),
    )?)
}

fn records(root: &Path) -> crate::Result<Vec<Operation>> {
    let chain = IndexedTail::open(&root.join(".editchain"))?;
    let mut records: Vec<_> = chain
        .chain()
        .shared_ops()
        .map(|op| {
            let OpKind::Activity(activity) = &op.kind else {
                return Err("capture wrote a legacy operation".into());
            };
            activity.validate()?;
            Ok(activity.as_ref().clone())
        })
        .collect::<crate::Result<_>>()?;
    records.sort_by_key(|record| record.id);
    Ok(records)
}

fn activity<'a>(
    operations: &'a [Operation],
    sequence: u64,
    lane: &str,
) -> crate::Result<&'a Operation> {
    operations
        .iter()
        .find(|op| op.id == identity::operation(SESSION, sequence, lane))
        .ok_or_else(|| "missing captured operation".into())
}

fn bytes(reader: &BlobReader, payload: &Payload) -> crate::Result<Vec<u8>> {
    match payload {
        Payload::Inline(bytes) => Ok(bytes.clone()),
        Payload::Blob(blob) => reader
            .resolve_content(blob.id)
            .ok_or_else(|| "missing captured bytes".into()),
        Payload::Empty => Err("missing payload is distinct from empty bytes".into()),
    }
}

fn file_payload(operation: &Operation) -> Option<&editchain_core::activity::File> {
    if let Kind::File(file) = &operation.kind {
        Some(file)
    } else {
        None
    }
}

fn note_payload(operation: &Operation) -> Option<&editchain_core::activity::Note> {
    if let Kind::Note(note) = &operation.kind {
        Some(note)
    } else {
        None
    }
}

fn original_payload(operation: &Operation) -> Option<&editchain_core::activity::Original> {
    if let Kind::Original(original) = &operation.kind {
        Some(original)
    } else {
        None
    }
}

fn replacement(edit: &FileEdit) -> Option<(&ByteRange, &Payload)> {
    if let FileEdit::ReplaceBytes { range, bytes } = edit {
        Some((range, bytes))
    } else {
        None
    }
}

#[test]
fn unsaved_revisions_units_context_exposure_and_late_attribution_survive_retries() {
    let root = tempfile::tempdir().expect("capture test data");
    let context = event(
        2,
        json!({"type":"workspace_context","observed_ms":1500,"workspace_path":root.path(),
        "repositories":[{"repository":"42","root":root.path(),"head":"0123456789012345678901234567890123456789"}]}),
    );
    let events = vec![
        start(),
        context,
        snapshot(3),
        change(4),
        event(
            5,
            json!({"type":"human_edit_batch", "group":4,"edits":[{"change":4,"signal":"keyboard_selection"}]}),
        ),
        event(6, json!({"type":"document_saved","document":document(2)})),
        event(
            7,
            json!({"type":"code_read", "document":document(2),"editor":"1", "ranges":[{"start":[0,1],"end":[0,2]}],"started_ms":6000,"duration_ms":500}),
        ),
        event(8, json!({"type":"tracking_gap","reason":"paused"})),
        event(9, json!({"type":"tracking_stopped"})),
    ];
    let request = batch(root.path(), &events).expect("capture test data");
    let response = CaptureWriter::default()
        .record_json(&request)
        .expect("capture test data");
    assert_eq!(
        response.get("accepted"),
        Some(&json!(9)),
        "all input events acknowledged"
    );
    assert_eq!(
        response.get("operation_schema"),
        Some(&json!(3)),
        "explicit operation version"
    );
    let before = records(root.path()).expect("capture test data");
    let retry = CaptureWriter::default()
        .record_json(&request)
        .expect("capture test data");
    assert_eq!(
        retry.get("replayed"),
        Some(&json!(9)),
        "restarted writer recognizes exact retries"
    );
    assert_eq!(
        records(root.path()).expect("capture test data"),
        before,
        "retry does not duplicate or mutate history"
    );
    let snapshot = activity(&before, 3, "activity").expect("capture test data");
    let changed = activity(&before, 4, "activity").expect("capture test data");
    let file = file_payload(changed).expect("file change");
    assert_eq!(
        file.action,
        FileAction::Change,
        "unsaved input is a change, not a save"
    );
    assert_eq!(file.revision, Some(changed.item), "explicit revision item");
    assert_ne!(
        changed.id, changed.item.0,
        "immutable event and logical item have separate identities"
    );
    assert_eq!(
        changed.id.to_string().len(),
        64,
        "retain full observation identity"
    );
    assert!(
        changed.causes.contains(&snapshot.item),
        "source revision is an explicit logical cause"
    );
    assert!(
        changed.parents.contains(
            &activity(&before, 2, "activity")
                .expect("capture test data")
                .id
        ),
        "recorded Git context is attached"
    );
    assert_eq!(
        changed.author, None,
        "recording a change does not establish its author"
    );
    let edit = file
        .text_edits
        .first()
        .ok_or("missing text edit")
        .expect("capture test data");
    assert_eq!(
        (edit.offset_utf16, edit.length_utf16),
        (1, 2),
        "emoji occupies two UTF-16 units"
    );
    let reader = BlobReader::open(&root.path().join(".editchain")).expect("capture test data");
    let (range, replacement) = replacement(&file.edit).expect("byte replacement");
    assert_eq!(
        *range,
        ByteRange { start: 0, end: 8 },
        "byte replacement spans exact UTF-8 source bytes"
    );
    assert_eq!(
        bytes(&reader, replacement).expect("capture test data"),
        "aé\r\nz".as_bytes(),
        "unsaved destination is exact"
    );
    let note = activity(&before, 5, "activity").expect("capture test data");
    let attribution = note_payload(note).expect("attribution note");
    assert_eq!(
        attribution.targets,
        vec![changed.id],
        "late attribution targets the exact earlier change"
    );
    assert_eq!(
        attribution.items,
        vec![changed.item],
        "late attribution also retains its revision item"
    );
    assert!(
        note.author.is_some() && note.author != Some(note.recorder),
        "contributor and recorder are distinct"
    );
    let read = activity(&before, 7, "activity").expect("capture test data");
    let read = file_payload(read).expect("read observation");
    assert_eq!(
        read.revision, file.revision,
        "exposure belongs to the recorded revision"
    );
    assert_eq!(
        read.ranges,
        vec![ByteRange { start: 1, end: 3 }],
        "exposure byte coordinates retain multibyte text"
    );
    assert_eq!(
        read.text_ranges
            .first()
            .map(|range| (range.start, range.end)),
        Some(([0, 1], [0, 2])),
        "native coordinates also retained"
    );
    assert!(
        matches!(&activity(&before, 8, "activity").expect("capture test data").kind, Kind::Note(note) if note.category == NoteKind::Gap),
        "gaps remain explicit"
    );
}

#[test]
fn raw_slices_archive_replay_and_blob_repair_are_exact() {
    let root = tempfile::tempdir().expect("capture test data");
    let raw = serde_json::to_string_pretty(&start()).expect("capture test data");
    let prefix =
        serde_json::to_string(&json!({"workspace_path":root.path(),"chain_dir":".editchain"}))
            .expect("capture test data");
    let request = format!(
        "{},\"events\":[{raw}]}}",
        prefix
            .strip_suffix('}')
            .ok_or("fixture envelope")
            .expect("capture test data")
    );
    let mut writer = CaptureWriter::default();
    let _response = writer
        .record_json(request.as_bytes())
        .expect("capture test data");
    let operations = records(root.path()).expect("capture test data");
    let original = original_payload(activity(&operations, 1, "raw").expect("capture test data"))
        .expect("original");
    let reader = BlobReader::open(&root.path().join(".editchain")).expect("capture test data");
    assert_eq!(
        bytes(&reader, &original.bytes).expect("capture test data"),
        raw.as_bytes(),
        "Original retains exact JSON whitespace"
    );
    fs::remove_dir_all(root.path().join(".editchain/blobs")).expect("capture test data");
    let _response = writer
        .record_json(request.as_bytes())
        .expect("capture test data");
    assert_eq!(
        bytes(&reader, &original.bytes).expect("capture test data"),
        raw.as_bytes(),
        "duplicate delivery repairs missing content before acknowledgement"
    );
    let archive = format!(
        "{{\"format\":\"editchain-human-history\",\"schema\":1,\"workspace_path\":{},\"event\":{raw}}}\n",
        serde_json::to_string(root.path()).expect("capture test data")
    );
    let ack = writer
        .record_archive_line(archive.as_bytes(), ".editchain")
        .expect("capture test data");
    assert_eq!(
        ack.get("replayed"),
        Some(&json!(1)),
        "overlapping archive replay has the same identity and raw bytes"
    );
    let _ack = writer
        .record_archive_line(archive.as_bytes(), ".rebuilt")
        .expect("capture test data");
    let rebuilt = IndexedTail::open(&root.path().join(".rebuilt")).expect("capture test data");
    let ids: BTreeSet<_> = rebuilt.chain().shared_ops().map(|op| op.id).collect();
    assert_eq!(
        ids,
        operations.iter().map(|op| op.id).collect(),
        "archive reconstructs stable observation identities"
    );
}

#[test]
fn archives_from_other_platforms_keep_source_paths_and_replay_into_a_local_chain() {
    for source in [
        r"C:\work\repo",
        r"\\server\share\repo",
        "/original/checkout",
    ] {
        let root = tempfile::tempdir().expect("local archive destination");
        let destination = root.path().join(".editchain");
        let events = [
            start(),
            event(
                2,
                json!({"type":"workspace_context", "observed_ms":1500,
                "workspace_path":source, "repositories":[{"repository":"42", "root":source, "head":null}]}),
            ),
            snapshot(3),
            change(4),
        ];
        let mut writer = CaptureWriter::default();
        for value in &events {
            let raw = serde_json::to_string_pretty(value).expect("raw event slice");
            let line = format!(
                "{{\"format\":\"editchain-human-history\",\"schema\":1,\"workspace_path\":{},\"event\":{raw}}}\n",
                serde_json::to_string(source).expect("source workspace")
            );
            let result = writer
                .record_archive_line(line.as_bytes(), destination.to_str().expect("local path"))
                .expect("portable replay");
            assert_eq!(
                result.get("accepted"),
                Some(&json!(1)),
                "source path never selects the destination"
            );
            let retry = CaptureWriter::default()
                .record_archive_line(line.as_bytes(), destination.to_str().expect("local path"))
                .expect("portable retry");
            assert_eq!(
                retry.get("replayed"),
                Some(&json!(1)),
                "archive retry uses the original identity"
            );
            let recorded = records(root.path()).expect("local records");
            let sequence = value
                .get("sequence")
                .and_then(Value::as_u64)
                .expect("event sequence");
            let original =
                original_payload(activity(&recorded, sequence, "raw").expect("source record"))
                    .expect("original");
            let reader = BlobReader::open(&destination).expect("local blobs");
            assert_eq!(
                bytes(&reader, &original.bytes).expect("stored source bytes"),
                raw.as_bytes(),
                "original paths and JSON formatting survive replay"
            );
            if !Path::new(source).is_absolute() {
                assert!(
                    writer
                        .record_archive_line(line.as_bytes(), ".relative")
                        .is_err(),
                    "foreign workspaces require an explicit local absolute destination"
                );
            }
        }
        let recorded = records(root.path()).expect("local records");
        assert!(
            file_payload(activity(&recorded, 4, "activity").expect("change")).is_some(),
            "source context does not block the following file change"
        );
    }
}

#[test]
fn recorded_paths_accept_native_absolute_forms_without_accepting_relative_paths() {
    for root in [
        "/checkout",
        "C:/work/repo",
        r"D:\work\repo",
        r"\\server\share\repo",
    ] {
        assert!(
            crate::wire::absolute_source_path(root),
            "valid absolute source {root}"
        );
    }
    for root in [
        "",
        "relative",
        "../work",
        "C:relative",
        r"\relative",
        r"\\server",
        r"\\server\",
        "/bad\0path",
    ] {
        assert!(
            !crate::wire::absolute_source_path(root),
            "invalid source {root}"
        );
    }
    let root = tempfile::tempdir().expect("local archive destination");
    let invalid_context = event(
        2,
        json!({"type":"workspace_context", "observed_ms":1500,
        "workspace_path":"C:relative", "repositories":[]}),
    );
    assert!(
        CaptureWriter::default()
            .record_json(&batch(root.path(), &[start(), invalid_context]).expect("batch"))
            .is_err(),
        "relative source context remains invalid"
    );
    let line = serde_json::to_vec(&json!({"format":"editchain-human-history", "schema":1,
        "workspace_path":"C:relative", "event":start()}))
    .expect("archive");
    assert!(
        CaptureWriter::default()
            .record_archive_line(&line, root.path().to_str().expect("local path"))
            .is_err(),
        "an absolute destination does not legitimize an invalid source path"
    );
}

#[test]
fn large_multi_cursor_edits_preserve_every_replacement_and_allow_following_events() {
    let count = 100_001_u32;
    let length = usize::try_from(count).expect("cursor count");
    let before = "😀".repeat(length);
    let after = "😀x".repeat(length);
    let changes: Vec<_> = (1..=count)
        .rev()
        .map(|offset| json!({"offset":offset.saturating_mul(2), "length":0, "text":"x"}))
        .collect();
    let events = [
        start(),
        event(
            2,
            json!({"type":"document_snapshot", "document":document(1), "text":before}),
        ),
        event(
            3,
            json!({"type":"document_changed", "document":document(2), "before_version":1,
            "before":before, "after":after, "changes":changes, "reason":null}),
        ),
        event(4, json!({"type":"document_saved", "document":document(2)})),
    ];
    let root = tempfile::tempdir().expect("multi-cursor chain");
    let request = batch(root.path(), &events).expect("batch");
    let result = CaptureWriter::default()
        .record_json(&request)
        .expect("large edit admission");
    assert_eq!(
        result.get("accepted"),
        Some(&json!(4)),
        "large edits cannot strand the following save"
    );
    let result = CaptureWriter::default()
        .record_json(&request)
        .expect("large edit retry");
    assert_eq!(
        result.get("replayed"),
        Some(&json!(4)),
        "durable large batches recover after restart"
    );
    let recorded = records(root.path()).expect("recorded multi-cursor change");
    let file = file_payload(activity(&recorded, 3, "activity").expect("change record"))
        .expect("file change");
    assert_eq!(
        file.text_edits.len(),
        length,
        "every native replacement survives conversion"
    );
    for (edit, offset) in file.text_edits.iter().zip((1..=count).rev()) {
        assert_eq!(
            edit.offset_utf16,
            u64::from(offset).saturating_mul(2),
            "UTF-16 offsets keep emitted order"
        );
    }
    let reader = BlobReader::open(&root.path().join(".editchain")).expect("change blobs");
    let (_, replacement) = replacement(&file.edit).expect("replacement bytes");
    assert_eq!(
        bytes(&reader, replacement).expect("after bytes"),
        after.as_bytes(),
        "the complete Unicode result survives"
    );
}

#[test]
fn reordered_batches_recover_without_skipping_predecessors_or_using_hash_order() {
    let root = tempfile::tempdir().expect("capture test data");
    let mut writer = CaptureWriter::default();
    assert!(
        writer
            .record_json(&batch(root.path(), &[snapshot(2)]).expect("capture test data"))
            .is_err(),
        "a missing start cannot be acknowledged"
    );
    assert!(
        writer
            .record_json(&batch(root.path(), &[start(), change(3)]).expect("capture test data"))
            .is_err(),
        "partial batch keeps the missing sequence explicit"
    );
    let response = writer
        .record_json(
            &batch(root.path(), &[start(), snapshot(2), change(3)]).expect("capture test data"),
        )
        .expect("capture test data");
    assert_eq!(
        response.get("replayed"),
        Some(&json!(1)),
        "retry repairs a partially admitted batch"
    );
    assert_eq!(
        response.get("accepted"),
        Some(&json!(2)),
        "only missing events append"
    );
    let first = records(root.path()).expect("capture test data");
    // Deliver independent sessions in descending hash order. Source sequence,
    // rather than maximum operation identity, still admits every lower ID.
    let mut sessions: Vec<_> = (1..=8)
        .map(|n| format!("{n:08}-1111-4111-8111-123456789abc"))
        .collect();
    sessions.sort_by_key(|session| std::cmp::Reverse(identity::operation(session, 1, "raw")));
    for session in sessions {
        let mut event = start();
        let _old = event
            .as_object_mut()
            .ok_or("fixture event")
            .expect("capture test data")
            .insert("session".into(), json!(session));
        let _ack = writer
            .record_json(&batch(root.path(), &[event]).expect("capture test data"))
            .expect("capture test data");
    }
    assert_eq!(
        records(root.path()).expect("capture test data").len(),
        first.len().saturating_add(32),
        "late lower IDs remain visible"
    );
}

#[test]
fn conflicts_remain_recorded_and_cannot_receive_durable_acknowledgements() {
    let root = tempfile::tempdir().expect("capture test data");
    let mut writer = CaptureWriter::default();
    let original = batch(root.path(), &[start()]).expect("capture test data");
    let _ack = writer.record_json(&original).expect("capture test data");
    let mut alternate = start();
    let _old = alternate
        .as_object_mut()
        .ok_or("fixture event")
        .expect("capture test data")
        .insert("time_ms".into(), json!(1234));
    assert!(
        writer
            .record_json(&batch(root.path(), &[alternate]).expect("capture test data"))
            .is_err(),
        "same source identity cannot change bytes"
    );
    let chain = IndexedTail::open(&root.path().join(".editchain")).expect("capture test data");
    let id = identity::operation(SESSION, 1, "raw");
    assert_eq!(
        chain.chain().record_locations(id).count(),
        2,
        "both exact source variants are retained"
    );
    assert!(
        chain.chain().get(id).is_none(),
        "conflicted identity is quarantined"
    );
    assert!(
        writer.record_json(&original).is_err(),
        "even an exact variant retry remains disputed"
    );
    assert!(
        writer
            .record_json(&batch(root.path(), &[snapshot(2)]).expect("capture test data"))
            .is_err(),
        "new work cannot bridge a disputed predecessor"
    );
}

#[test]
fn revisions_and_receipts_cannot_refer_to_unrelated_or_missing_buffers() {
    let root = tempfile::tempdir().expect("capture test data");
    let mut writer = CaptureWriter::default();
    let _ack = writer
        .record_json(&batch(root.path(), &[start(), snapshot(2)]).expect("capture test data"))
        .expect("capture test data");
    let invalid = event(3, json!({"type":"human_edit", "change":2, "signal":"undo"}));
    assert!(
        writer
            .record_json(&batch(root.path(), &[invalid]).expect("capture test data"))
            .is_err(),
        "a snapshot is not a document change"
    );
    let mut wrong = change(3);
    let event = wrong
        .get_mut("event")
        .and_then(Value::as_object_mut)
        .ok_or("fixture change")
        .expect("capture test data");
    let _old = event.insert("before".into(), json!("b😀\r\nz"));
    let _old = event.insert("after".into(), json!("bé\r\nz"));
    assert!(
        writer
            .record_json(&batch(root.path(), &[wrong]).expect("capture test data"))
            .is_err(),
        "exact replay must still match the captured source revision"
    );
    let _ack = writer
        .record_json(&batch(root.path(), &[change(3)]).expect("capture test data"))
        .expect("capture test data");
}

#[test]
fn a_late_source_conflict_invalidates_the_cached_recorder_beyond_its_immediate_predecessor() {
    let root = tempfile::tempdir().expect("capture directory");
    let mut writer = CaptureWriter::default();
    let _ack = writer
        .record_json(
            &batch(root.path(), &[start(), snapshot(2), change(3)]).expect("ordered batch"),
        )
        .expect("initial capture");
    let mut conflicting = snapshot(2);
    let _old = conflicting
        .get_mut("event")
        .and_then(Value::as_object_mut)
        .expect("snapshot body")
        .insert("text".into(), json!("different recorded baseline"));
    assert!(
        writer
            .record_json(&batch(root.path(), &[conflicting]).expect("alternate batch"))
            .is_err(),
        "late source variants remain disputed"
    );
    let saved = event(4, json!({"type":"document_saved", "document":document(2)}));
    assert!(
        writer
            .record_json(&batch(root.path(), &[saved]).expect("save batch"))
            .is_err(),
        "a still-present immediate predecessor cannot hide an earlier source conflict"
    );
    let mut fresh = start();
    let _old = fresh.as_object_mut().expect("new recorder").insert(
        "session".into(),
        json!("aaaaaaaa-1111-4111-8111-123456789abc"),
    );
    assert!(
        writer
            .record_json(&batch(root.path(), &[fresh]).expect("independent session"))
            .is_ok(),
        "unrelated recorder incarnations can continue"
    );
}

#[test]
fn legacy_archive_units_remain_readable_but_contradictory_units_and_short_reads_fail() {
    let root = tempfile::tempdir().expect("capture directory");
    let mut writer = CaptureWriter::default();
    let mut legacy = vec![start(), snapshot(2)];
    for event in &mut legacy {
        let object = event.as_object_mut().expect("legacy envelope");
        let _units = object.remove("units");
        let _identity = object.remove("identity");
    }
    let _old = legacy
        .first_mut()
        .and_then(|event| event.get_mut("event"))
        .and_then(Value::as_object_mut)
        .expect("legacy policy")
        .insert("activity_schema".into(), json!(2));
    let response = writer
        .record_json(&batch(root.path(), &legacy).expect("legacy batch"))
        .expect("legacy source conversion");
    assert_eq!(
        response.get("operation_schema"),
        Some(&json!(3)),
        "older raw source still emits schema three"
    );
    let mut read = event(
        3,
        json!({"type":"code_read", "document":document(1), "editor":"1", "ranges":[{"start":[0,0],"end":[0,1]}], "started_ms":2800,"duration_ms":200}),
    );
    let _identity = read
        .as_object_mut()
        .expect("reading envelope")
        .remove("identity");
    assert!(
        writer
            .record_json(&batch(root.path(), &[read.clone()]).expect("short read"))
            .is_err(),
        "a reading indicator must reach its recorded dwell policy"
    );
    let _old = read
        .get_mut("event")
        .and_then(Value::as_object_mut)
        .expect("read body")
        .insert("duration_ms".into(), json!(500));
    let _old = read
        .get_mut("units")
        .and_then(Value::as_object_mut)
        .expect("units")
        .insert("offsets".into(), json!("bytes"));
    assert!(
        writer
            .record_json(&batch(root.path(), &[read]).expect("contradictory units"))
            .is_err(),
        "byte offsets cannot be passed as native UTF-16 offsets"
    );
}
