use super::*;
use editchain_core::activity::{Author, AuthorRole, ItemId, Kind, Operation, OriginalRef};
use idle_editor_capture::CaptureWriter;
use schema3_regressions::{loaded, saved};

fn live_batch(root: &Path, lines: &[&[u8]]) -> Result<Vec<u8>> {
    #[derive(serde::Deserialize)]
    struct Archive<'a> {
        #[serde(borrow)]
        event: &'a serde_json::value::RawValue,
    }
    let mut events = Vec::new();
    for line in lines {
        let archive: Archive<'_> = serde_json::from_slice(line)?;
        events.push(archive.event.get());
    }
    Ok(format!(
        "{{\"workspace_path\":{},\"chain_dir\":\".editchain\",\"events\":[{}]}}",
        serde_json::to_string(root)?,
        events.join(",")
    )
    .into_bytes())
}

#[test]
fn human_archives_and_live_capture_share_exact_records_in_both_merge_orders() -> Result {
    let temporary = tempfile::tempdir()?;
    let source = temporary.path().join("source");
    std::fs::create_dir(&source)?;
    std::fs::write(source.join("session.jsonl"), HUMAN)?;
    let reference = temporary.path().join("reference");
    let lines = records(HUMAN);
    let _ack = CaptureWriter::default().record_json(&live_batch(&reference, &lines)?)?;
    let expected = loaded(&reference.join(".editchain"))?;

    for live_first in [true, false] {
        let root = temporary.path().join(if live_first {
            "live-first"
        } else {
            "archive-first"
        });
        let chain = root.join(".editchain");
        let bytes = live_batch(&root, &lines)?;
        let mut writer = CaptureWriter::default();
        if live_first {
            let _ack = writer.record_json(&bytes)?;
        }
        let mut blobs = FsBlobSink::new(chain.join("blobs"))?;
        let mut cursors = MemoryCursorStore::default();
        let batch = Provider::Human
            .capture(&source, &ImportOptions::default(), &mut blobs, &cursors)?
            .into_schema3(&mut blobs)?;
        for record in &expected {
            verify!(
                batch.operations().contains(record),
                "archive conversion must match every live record byte-for-byte"
            );
        }
        let mut log = LogStore::new(SegmentStore::open(&chain)?);
        let _report = batch.persist(&mut log, &mut cursors)?;
        drop(log);
        let ack = writer.record_json(&bytes)?;
        verify_eq!(
            ack.get("accepted").and_then(serde_json::Value::as_u64),
            Some(0),
            "a live replay adds no second event"
        );
        let merged = loaded(&chain)?;
        for record in &expected {
            verify_eq!(
                merged.iter().filter(|op| op.id == record.id).count(),
                1,
                "merged capture retains one canonical operation"
            );
        }
        verify_eq!(
            merged
                .iter()
                .filter_map(Operation::view)
                .filter(|record| matches!(record.kind, Kind::File(_)))
                .count(),
            2,
            "snapshot and edit each appear once"
        );
    }
    Ok(())
}

#[test]
fn resumed_human_import_uses_accepted_source_context_without_new_identities() -> Result {
    let temporary = tempfile::tempdir()?;
    let source = temporary.path().join("source");
    std::fs::create_dir(&source)?;
    let archive = source.join("session.jsonl");
    let parts = records(HUMAN);
    let prefix = parts.iter().take(2).copied().collect::<Vec<_>>().concat();
    std::fs::write(&archive, prefix)?;
    let chain = temporary.path().join("resumed");
    let mut blobs = FsBlobSink::new(chain.join("blobs"))?;
    let mut cursors = MemoryCursorStore::default();
    for complete in [false, true] {
        if complete {
            std::fs::write(&archive, HUMAN)?;
        }
        let batch = Provider::Human
            .capture(&source, &ImportOptions::default(), &mut blobs, &cursors)?
            .into_schema3(&mut blobs)?;
        let mut log = LogStore::new(SegmentStore::open(&chain)?);
        let _report = batch.persist(&mut log, &mut cursors)?;
    }
    let expected = Provider::Human
        .capture(
            &source,
            &ImportOptions::default(),
            &mut blobs,
            &MemoryCursorStore::default(),
        )?
        .into_schema3(&mut blobs)?;
    let resumed = loaded(&chain)?;
    verify_eq!(
        resumed.len(),
        expected.operations().len(),
        "resumption emits exactly the complete conversion"
    );
    for record in expected.operations() {
        verify!(
            resumed.contains(record),
            "accepted prefixes preserve exact later conversion bytes"
        );
    }
    Ok(())
}

#[test]
fn split_archives_reuse_accepted_context_for_utf16_reads_and_attribution() -> Result {
    use serde_json::json;
    let temporary = tempfile::tempdir()?;
    let source = temporary.path().join("source");
    std::fs::create_dir(&source)?;
    let first = *records(HUMAN).first().ok_or("missing recorder fixture")?;
    let template: serde_json::Value = serde_json::from_slice(first)?;
    let document = |version| json!({"id":"buffer-1","uri":"file:///workspace/notes.txt","path":"notes.txt","version":version});
    let bodies = [
        json!({"type":"tracking_started","dwell_ms":500,"vscode_version":"1.90.0","activity_schema":3}),
        json!({"type":"workspace_context","observed_ms":1000,"workspace_path":"/workspace","repositories":[]}),
        json!({"type":"document_snapshot","document":document(1),"text":"a😀\r\nz"}),
        json!({"type":"document_changed","document":document(2),"before_version":1,"before":"a😀\r\nz","after":"aé\r\nz","reason":null,"changes":[{"offset":1,"length":2,"text":"é"}]}),
        json!({"type":"human_edit_batch","group":4,"edits":[{"change":4,"signal":"keyboard_selection"}]}),
        json!({"type":"document_saved","document":document(2)}),
        json!({"type":"code_read","document":document(2),"editor":"1","ranges":[{"start":[0,1],"end":[0,2]}],"started_ms":6000,"duration_ms":500}),
        json!({"type":"tracking_stopped"}),
    ];
    let mut lines = Vec::new();
    for (offset, body) in bodies.into_iter().enumerate() {
        let mut archive = template.clone();
        let event = archive
            .get_mut("event")
            .and_then(serde_json::Value::as_object_mut)
            .ok_or("invalid recorder fixture")?;
        let sequence = offset.saturating_add(1);
        let _old = event.insert("sequence".into(), json!(sequence));
        let _old = event.insert("time_ms".into(), json!(sequence.saturating_mul(1000)));
        let _old = event.insert("event".into(), body);
        let mut bytes = serde_json::to_vec(&archive)?;
        bytes.push(b'\n');
        lines.push(bytes);
    }
    let (prefix, suffix) = lines.split_at(3);
    std::fs::write(source.join("z-start.jsonl"), prefix.concat())?;
    let chain = temporary.path().join("archive");
    let mut blobs = FsBlobSink::new(chain.join("blobs"))?;
    let mut cursors = MemoryCursorStore::default();
    for complete in [false, true] {
        if complete {
            // The newer file sorts before the already accepted baseline file.
            std::fs::write(source.join("a-tail.jsonl"), suffix.concat())?;
        }
        let batch = Provider::Human
            .capture(&source, &ImportOptions::default(), &mut blobs, &cursors)?
            .into_schema3(&mut blobs)?;
        let mut log = LogStore::new(SegmentStore::open(&chain)?);
        let _report = batch.persist(&mut log, &mut cursors)?;
    }
    let root = temporary.path().join("live");
    let lines = lines.iter().map(Vec::as_slice).collect::<Vec<_>>();
    let _ack = CaptureWriter::default().record_json(&live_batch(&root, &lines)?)?;
    let imported = loaded(&chain)?;
    for record in loaded(&root.join(".editchain"))? {
        verify!(
            imported.contains(&record),
            "split archive conversion matches live UTF-16, context and author records"
        );
    }
    Ok(())
}

#[test]
fn raw_human_import_keeps_partial_streams_without_deriving_an_edit() -> Result {
    let temporary = tempfile::tempdir()?;
    let parts = records(HUMAN);
    let change = parts.get(2).ok_or("missing change fixture")?;
    std::fs::write(temporary.path().join("session.jsonl"), change)?;
    let mut blobs = idle_history_import::MemoryBlobSink::default();
    let batch = Provider::Human
        .capture(
            temporary.path(),
            &ImportOptions {
                normalize: false,
                ..ImportOptions::default()
            },
            &mut blobs,
            &MemoryCursorStore::default(),
        )?
        .into_originals_schema3(&mut blobs, false)?;
    verify_eq!(
        batch.operations().len(),
        1,
        "raw capture retains only its exact archive record"
    );
    verify!(
        batch
            .operations()
            .iter()
            .filter_map(Operation::view)
            .all(|record| matches!(record.kind, Kind::Original(_))),
        "a partial stream has no fabricated baseline or edit"
    );
    Ok(())
}

#[test]
fn earlier_human_conversion_is_readable_and_requires_a_new_import_destination() -> Result {
    let temporary = tempfile::tempdir()?;
    let original = editchain_core::OpId::from_bytes([31; 32]);
    let id = editchain_core::OpId::from_bytes(blake3::derive_key(
        "editchain.human-author.v1",
        original.as_bytes(),
    ));
    let item = ItemId::derive("test.previous-human-author", b"author");
    let mut record = Operation::new(
        id,
        item,
        item,
        Kind::Author(Author {
            label: Payload::Inline(b"Earlier author".to_vec()),
            role: AuthorRole::Person,
            native_role: Payload::Empty,
            metadata: Payload::Empty,
        }),
    );
    record.original = Some(OriginalRef {
        operation: original,
        converter: idle_history_import::activity::CONTRACT.into(),
    });
    record.parents.push(original);
    let op = record.into_op()?;
    saved(temporary.path(), std::slice::from_ref(&op))?;
    verify!(
        !idle_history_import::activity::uses_migration_ids(temporary.path())?,
        "other provider imports keep the normal namespace without the human-only scan"
    );
    let error = idle_history_import::activity::validate_human_destination(temporary.path())
        .err()
        .ok_or("older human conversion was accepted")?;
    verify!(
        error.to_string().contains("older human archive conversion"),
        "the error identifies the required source replay"
    );
    verify_eq!(
        loaded(temporary.path())?,
        vec![op],
        "existing history remains readable and unchanged"
    );
    Ok(())
}
