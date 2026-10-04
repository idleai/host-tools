//! Length-prefixed capture RPC, executed on the file-owning extension host.

use idle_editor_capture::{CaptureWriter, observe_context};
use serde::Deserialize;
use serde_json::{Value, value::RawValue};
use std::{io, path::Path};
// Cargo supplies the package's library dependencies to this binary as well.
use {
    blake3 as _, editchain_core as _, editchain_git as _, editchain_store as _, idle_history as _,
};

#[cfg(test)]
use tempfile as _;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    id: u64,
    body: Body,
}

#[derive(Deserialize)]
enum Body {
    RecordEditorEvents(Box<RawValue>),
    GetEditorContext(Open),
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Open {
    workspace_path: String,
    chain_dir: String,
}

fn main() -> idle_editor_capture::Result<()> {
    let mut input = io::stdin().lock();
    let mut output = io::stdout().lock();
    let mut writer = CaptureWriter::default();
    while let Some(bytes) = idle_host_io::read_frame(&mut input, 160 * 1024 * 1024)? {
        let request: Request = serde_json::from_slice(&bytes)?;
        let result = handle(&mut writer, request.body);
        let body = match result {
            Ok(value) => serde_json::json!({"Ok": value}),
            Err(error) => {
                serde_json::json!({"Error": {"code": "capture_failed", "message": error.to_string()}})
            }
        };
        let response = serde_json::to_vec(&serde_json::json!({"id": request.id, "body": body}))?;
        idle_host_io::write_frame(&mut output, &response, usize::MAX)?;
    }
    Ok(())
}

fn handle(writer: &mut CaptureWriter, body: Body) -> idle_editor_capture::Result<Value> {
    match body {
        Body::RecordEditorEvents(raw) => writer.record_json(raw.get().as_bytes()),
        Body::GetEditorContext(open) => {
            if open.chain_dir.is_empty() {
                return Err("capture chain binding is required".into());
            }
            observe_context(Path::new(&open.workspace_path))
        }
    }
}
