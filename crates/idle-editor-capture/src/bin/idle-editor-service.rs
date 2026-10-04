//! Length-prefixed capture RPC, executed on the file-owning extension host.

use idle_editor_capture::{CaptureWriter, observe_context};
use serde::Deserialize;
use serde_json::{Value, value::RawValue};
use std::{
    io::{self, Read, Write},
    path::Path,
};
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
    loop {
        let mut header = [0; 4];
        if input.read(&mut header[..1])? == 0 {
            return Ok(());
        }
        input.read_exact(header.get_mut(1..).ok_or("missing frame header")?)?;
        let length = usize::try_from(u32::from_le_bytes(header))?;
        if length == 0 || length > 160 * 1024 * 1024 {
            return Err("invalid capture frame length".into());
        }
        let mut bytes = vec![0; length];
        input.read_exact(&mut bytes)?;
        let request: Request = serde_json::from_slice(&bytes)?;
        let result = handle(&mut writer, request.body);
        let body = match result {
            Ok(value) => serde_json::json!({"Ok": value}),
            Err(error) => {
                serde_json::json!({"Error": {"code": "capture_failed", "message": error.to_string()}})
            }
        };
        let response = serde_json::to_vec(&serde_json::json!({"id": request.id, "body": body}))?;
        output.write_all(&u32::try_from(response.len())?.to_le_bytes())?;
        output.write_all(&response)?;
        output.flush()?;
    }
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
