//! Framed repository reads for one immutable installation; no provider semantics in hosts.

use std::io;

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncRead, AsyncWrite};

use crate::{Binding, Credentials, Reader, Snapshot};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    id: u64,
    body: Read,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Read {
    credentials: Option<Credentials>,
    #[serde(default)]
    refresh_github: bool,
    #[serde(default)]
    local_only: bool,
}

#[derive(Serialize)]
struct Response {
    id: u64,
    body: Result<Snapshot, String>,
}

/// Serve bounded length-prefixed JSON while the host owns the process.
/// Each request supplies the current host credential; no secret is a process argument.
///
/// # Errors
/// Returns invalid installation, framing, output size or transport errors.
pub async fn serve(
    input: impl AsyncRead + Unpin,
    mut output: impl AsyncWrite + Unpin,
    binding: Binding,
) -> io::Result<()> {
    let mut service = Service::new(binding)?;
    let mut input = input;
    while let Some(bytes) = idle_host_io::asynchronous::read_frame(&mut input, 16 * 1024).await? {
        let bytes = service.handle(&bytes).await?;
        idle_host_io::asynchronous::write_frame(&mut output, &bytes, 8 * 1024 * 1024).await?;
    }
    Ok(())
}

/// Repository reads for one immutable host binding, independently of transport ownership.
#[derive(Debug)]
pub struct Service {
    reader: Reader,
}

impl Service {
    /// Validate the installation and prepare its provider clients.
    /// # Errors
    /// Rejects invalid repository paths or client initialization failures.
    pub fn new(binding: Binding) -> io::Result<Self> {
        Ok(Self {
            reader: Reader::new(binding)?,
        })
    }

    /// Execute one bounded read using only the credential supplied with this request.
    /// # Errors
    /// Returns malformed requests or replies exceeding the transport limit.
    pub async fn handle(&mut self, bytes: &[u8]) -> io::Result<Vec<u8>> {
        if bytes.len() > 16 * 1024 {
            return Err(io::Error::other(
                "Repository request exceeds the request limit.",
            ));
        }
        let request: Request = serde_json::from_slice(bytes)
            .map_err(|_error| io::Error::other("Invalid repository request."))?;
        let result = if request.body.local_only {
            self.reader.read_local().await
        } else {
            self.reader
                .read(
                    request.body.credentials.as_ref(),
                    request.body.refresh_github,
                )
                .await
        };
        let response = Response {
            id: request.id,
            body: result.map_err(|error| error.to_string()),
        };
        let bytes = serde_json::to_vec(&response).map_err(io::Error::other)?;
        if bytes.len() > 8 * 1024 * 1024 {
            return Err(io::Error::other(
                "Repository snapshot exceeds the response limit.",
            ));
        }
        Ok(bytes)
    }
}

#[cfg(test)]
mod tests {
    use idle_protocol::v1::repository::RepositoryScope;
    use serde_json::{Value, json};

    use super::{Binding, Service};

    #[tokio::test]
    async fn local_reads_are_opt_in_and_existing_requests_remain_valid() {
        let directory = tempfile::tempdir().expect("temporary binding");
        let scope = RepositoryScope {
            workspace_id: "workspace".into(),
            repository_id: "repository".into(),
            chain: "chain".into(),
        };
        let binding = Binding {
            scope: scope.clone(),
            root: directory.path().into(),
            chain_directory: directory.path().join("chain"),
        };
        let mut service = Service::new(binding).expect("repository service");
        for (id, body) in [
            (1, json!({"credentials":null})),
            (2, json!({"credentials":null, "local_only":true})),
        ] {
            let reply = service
                .handle(&serde_json::to_vec(&json!({"id":id,"body":body})).expect("request"))
                .await
                .expect("compatible read");
            let reply: Value = serde_json::from_slice(&reply).expect("response");
            assert_eq!(reply.get("id"), Some(&json!(id)));
            assert_eq!(
                reply.pointer("/body/Ok/repository/scope"),
                Some(&json!(scope))
            );
        }
        assert!(
            service
                .handle(br#"{"id":3,"body":{"credentials":null,"arbitrary":true}}"#)
                .await
                .is_err()
        );
    }
}
