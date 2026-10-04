//! Generate synthetic engine records for the packaged native-preview smoke check.

use std::{io, path::PathBuf};
use {editchain_core as _, idle_repository as _};

use editchain_engine::{
    ContentId, Engine, FileEdit, Op, OpId, PathId, Payload,
    activity::{
        File, FileAction, ItemId, Kind, Operation, Original, OriginalRef, Session, SessionAction,
    },
};
use idle_history::{binding::RepositoryChainBinding, query::RecordRef};
use idle_history_native::history::{Request, Source, Target};
use {idle_editor_capture as _, idle_history_import as _, idle_protocol as _, serde as _};

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

fn id(byte: u8) -> OpId {
    OpId::from_bytes([byte; 32])
}

fn file(byte: u8, before: ContentId, after: ContentId) -> Result<Op> {
    let mut operation = Operation::new(
        id(byte),
        ItemId(id(byte)),
        ItemId(id(250)),
        Kind::File(File {
            action: FileAction::Change,
            path: PathId(1),
            name: Payload::Inline(b"recorded.ts".to_vec()),
            renamed_to: None,
            revision: Some(ItemId(id(byte))),
            before: Some(before),
            after: Some(after),
            edit: FileEdit::None,
            text_edits: Vec::new(),
            change: None,
            caused_by: None,
            ranges: Vec::new(),
            text_ranges: Vec::new(),
            duration_ms: None,
        }),
    );
    operation.session = Some(ItemId(id(5)));
    operation.original = Some(OriginalRef {
        operation: id(1),
        converter: "smoke-v1".into(),
    });
    Ok(operation.into_op()?)
}

fn main() -> Result {
    let root = PathBuf::from(
        std::env::args_os()
            .nth(1)
            .ok_or("fixture directory required")?,
    );
    let engine = Engine::open(root.join("chain"))?;
    let raw = b"\xff\x00{ \"raw\": true } \r\n";
    let original = Operation::new(
        id(1),
        ItemId(id(1)),
        ItemId(id(250)),
        Kind::Original(Original {
            provider: "smoke".into(),
            format: None,
            native: Vec::new(),
            location: None,
            bytes: Payload::Inline(raw.to_vec()),
            hash: None,
        }),
    )
    .into_op()?;
    let _stored = engine.append(&original)?;
    let before = "\u{feff}a😀\r\nz\n".as_bytes();
    let after = b"a\xc3\xa9\r\nz  ";
    let base = engine.store_blob(before)?.id;
    let result = engine.store_blob(after)?.id;
    let operation = file(2, base, result)?;
    let encoded = editchain_engine::encode_op(&operation)?;
    let _stored = engine.append_encoded(&encoded)?;
    let empty = engine.store_blob(b"")?.id;
    let _stored = engine.append(&file(3, base, empty)?)?;
    let elsewhere = tempfile::tempdir()?;
    let missing = Engine::open(elsewhere.path())?.store_blob(b"missing")?.id;
    let _stored = engine.append(&file(4, base, missing)?)?;
    let mut session = Operation::new(
        id(5),
        ItemId(id(5)),
        ItemId(id(250)),
        Kind::Session(Session {
            action: SessionAction::Started,
            label: Payload::Inline(b"Imported smoke session".to_vec()),
            settings: Payload::Empty,
            participants: Vec::new(),
            parent: None,
            initiated_by: None,
        }),
    );
    session.session = Some(ItemId(id(5)));
    session.original = Some(OriginalRef {
        operation: id(1),
        converter: "smoke-v1".into(),
    });
    let _stored = engine.append(&session.into_op()?)?;
    let github_raw = br#"[{"number":42,"title":"Fixture issue","labels":[{"name":"triage"},{"name":"needs-input"}]}]"#;
    let github = Operation::new(
        id(6),
        ItemId(id(6)),
        ItemId(id(250)),
        Kind::Original(Original {
            provider: "github".into(),
            format: Some("rest-json-2026-03-10".into()),
            native: Vec::new(),
            location: None,
            bytes: Payload::Inline(github_raw.to_vec()),
            hash: None,
        }),
    )
    .into_op()?;
    let _stored = engine.append(&github)?;
    let queries = engine.queries()?;
    let github_records = queries.record_variants(id(6))?;
    let github_record = github_records
        .first()
        .ok_or("GitHub fixture record")?
        .reference;
    let github_source = serde_json::json!({"observation":id(6).to_string(),"item":id(6).to_string(),"record_hash":OpId::from_bytes(github_record.record_hash).to_string()});
    let binding = RepositoryChainBinding {
        workspace_id: "smoke".into(),
        repository_id: "repository".into(),
        chain: "logical-chain".into(),
    };
    let mut requests = Vec::new();
    for (operation, target) in [
        (id(2), Target::File),
        (id(2), Target::Diff),
        (id(2), Target::Record),
        (id(2), Target::Original),
        (id(3), Target::File),
        (id(4), Target::File),
    ] {
        let records = queries.record_variants(operation)?;
        let record = records.first().ok_or("fixture record")?.reference;
        requests.push(Request {
            binding: binding.clone(),
            source: Source::Current,
            target,
            record: RecordRef {
                operation: operation.to_string(),
                hash: OpId::from_bytes(record.record_hash).to_string(),
            },
        });
    }
    let fixture = serde_json::json!({ "binding": binding, "requests": requests, "before": before, "after": after, "raw": raw.as_slice(), "encoded": encoded, "session_id":id(5).to_string(), "github_source":github_source, "github_raw":github_raw.as_slice() });
    std::fs::write(
        root.join("history.json"),
        serde_json::to_vec(&fixture).map_err(io::Error::other)?,
    )?;
    Ok(())
}
