//! Editor capture operations shared by standalone and multiplexed native hosts.

use crate::{CaptureWriter, observe_context};
use serde::{Deserialize, Serialize};
use serde_json::{Value, value::RawValue};
use std::{io, path::PathBuf};

/// The file-owning host installs this scope before admitting observations.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    /// Absolute workspace root, independent of the host process's working directory.
    pub workspace_path: PathBuf,
    /// Absolute current-chain directory selected for this workspace.
    pub chain_dir: PathBuf,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    id: u64,
    body: Body,
}

#[derive(Deserialize)]
enum Body {
    RecordEditorEvents(Box<RawValue>),
    GetEditorContext(Binding),
}

#[derive(Deserialize)]
struct Paths {
    workspace_path: PathBuf,
    chain_dir: PathBuf,
}

/// One ordered writer with an optional immutable host-installed scope.
#[derive(Debug, Default)]
pub struct Service {
    writer: CaptureWriter,
    binding: Option<Binding>,
}

impl Service {
    /// Bind capture to absolute workspace and chain paths.
    /// # Errors
    /// Rejects relative or empty paths before accepting observations.
    pub fn new(binding: Binding) -> io::Result<Self> {
        if !binding.workspace_path.is_absolute() || !binding.chain_dir.is_absolute() {
            return Err(io::Error::other("capture bindings require absolute paths"));
        }
        Ok(Self {
            writer: CaptureWriter::default(),
            binding: Some(binding),
        })
    }

    /// Admit one request, retaining original event JSON bytes and durable acknowledgement.
    /// # Errors
    /// Returns malformed requests or response serialization failures.
    pub fn handle(&mut self, bytes: &[u8]) -> io::Result<Vec<u8>> {
        if bytes.len() > 160 * 1024 * 1024 {
            return Err(io::Error::other("capture request exceeds its limit"));
        }
        let request: Request = serde_json::from_slice(bytes).map_err(io::Error::other)?;
        let result = self.execute(request.body);
        let body = match result {
            Ok(value) => serde_json::json!({"Ok": value}),
            Err(error) => {
                serde_json::json!({"Error": {"code": "capture_failed", "message": error.to_string()}})
            }
        };
        serde_json::to_vec(&serde_json::json!({"id": request.id, "body": body}))
            .map_err(io::Error::other)
    }

    fn execute(&mut self, body: Body) -> crate::Result<Value> {
        match body {
            Body::RecordEditorEvents(raw) => {
                let paths: Paths = serde_json::from_str(raw.get())?;
                self.validate(&paths)?;
                self.writer.record_json(raw.get().as_bytes())
            }
            Body::GetEditorContext(paths) => {
                self.validate(&Paths {
                    workspace_path: paths.workspace_path.clone(),
                    chain_dir: paths.chain_dir,
                })?;
                observe_context(&paths.workspace_path)
            }
        }
    }

    fn validate(&self, paths: &Paths) -> crate::Result<()> {
        if !paths.workspace_path.is_absolute() || paths.chain_dir.as_os_str().is_empty() {
            return Err(
                "capture requires an absolute workspace and explicit chain directory".into(),
            );
        }
        if self.binding.as_ref().is_some_and(|binding| {
            paths.workspace_path != binding.workspace_path
                || paths.workspace_path.join(&paths.chain_dir) != binding.chain_dir
        }) {
            return Err("capture request belongs to a different workspace or chain".into());
        }
        Ok(())
    }
}

/// Serve the original standalone capture protocol until input closes.
/// # Errors
/// Returns malformed framing, requests or transport failures.
pub fn serve(mut input: impl io::Read, mut output: impl io::Write) -> io::Result<()> {
    let mut service = Service::default();
    while let Some(bytes) = idle_host_io::read_frame(&mut input, 160 * 1024 * 1024)? {
        let response = service.handle(&bytes)?;
        idle_host_io::write_frame(&mut output, &response, 160 * 1024 * 1024)?;
    }
    Ok(())
}
