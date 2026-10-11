//! Daemon-owned standalone authority over a private, bounded helper pipe.

use std::{io, path::PathBuf, sync::Arc};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_util::sync::CancellationToken;

use crate::{
    Error, Result,
    authority::{
        Authority, Principal,
        runtime_transfer::{
            RuntimeChunk, RuntimeDestination, RuntimeReceipt, TRANSFER_CHUNK_BYTES,
        },
    },
    clock::SystemClock,
    invitation::Secret,
    persistence::{FilePersistence, MAX_STATE_BYTES, Persistence},
    service::Command,
};

use super::framing;

/// Operations admitted only after Codex validates a coordination-owner grant.
#[derive(Debug, Deserialize, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    /// Read the destination and its durable import acknowledgement.
    Status,
    /// Upload one bounded package slice. Offset zero restarts an incomplete upload.
    Upload {
        /// Identity and digest selected before the source froze.
        receipt: RuntimeReceipt,
        /// Total decoded package size.
        total: usize,
        /// Decoded byte offset.
        offset: usize,
        /// Base64 encoded private bytes.
        content: Secret,
    },
    /// Atomically install a completely received package, or return its earlier receipt.
    Commit {
        /// Expected immutable acknowledgement.
        receipt: RuntimeReceipt,
    },
    /// Execute the existing standalone protocol as the authenticated owner.
    Call {
        /// Existing coordination command; history lifecycle calls are refused.
        command: Command,
    },
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Start {
    version: u32,
    state_directory: PathBuf,
    destination: RuntimeDestination,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Call {
    client_id: String,
    request: Request,
}

struct Upload {
    client_id: String,
    receipt: RuntimeReceipt,
    total: usize,
    bytes: Vec<u8>,
}

struct Host {
    storage: Arc<FilePersistence>,
    destination: RuntimeDestination,
    authority: Option<Authority>,
    upload: Option<Upload>,
}

/// Hold one authority independently of clients, releasing its lock on daemon EOF.
/// Paths and authenticated client IDs must come from the daemon, never RPC bodies.
/// # Errors
/// Reports incompatible framing, conflicting owners and broken private pipes.
pub async fn serve_authority(
    mut input: impl AsyncRead + Unpin + Send,
    mut output: impl AsyncWrite + Unpin + Send,
    cancel: &CancellationToken,
) -> io::Result<()> {
    framing::write(&mut output, &json!({"version":1})).await?;
    let start: Start = tokio::select! {
        () = cancel.cancelled() => return Ok(()),
        start = framing::read(&mut input) => start?.ok_or_else(framing::invalid)?,
    };
    if start.version != 1
        || !start.state_directory.is_absolute()
        || !start.destination.checkout_root.is_absolute()
    {
        return Err(framing::invalid());
    }
    let storage = Arc::new(FilePersistence::open(start.state_directory).map_err(safe_error)?);
    let authority = if storage.load("authority").map_err(safe_error)?.is_some() {
        Some(
            Authority::open_runtime(storage.clone(), Arc::new(SystemClock), &start.destination)
                .map_err(safe_error)?,
        )
    } else {
        None
    };
    let mut host = Host {
        storage,
        destination: start.destination,
        authority,
        upload: None,
    };
    framing::write(&mut output, &json!({"ready":true})).await?;
    loop {
        let call: Option<Call> = tokio::select! {
            () = cancel.cancelled() => return Ok(()),
            call = framing::read(&mut input) => call?,
        };
        let Some(call) = call else {
            return Ok(());
        };
        // File operations are serialized with the authority's lifetime, away from
        // the app-server's request and transport loops.
        let result = host.call(&call.client_id, call.request);
        let result = if serde_json::to_vec(&result)
            .map_err(|_error| framing::invalid())?
            .len()
            > 500 * 1024
        {
            Err(Error::Busy)
        } else {
            result
        };
        framing::write(&mut output, &result).await?;
    }
}

impl Host {
    fn call(&mut self, client_id: &str, request: Request) -> Result<Value> {
        if let Some(authority) = &self.authority
            && authority.runtime_owner()?.1.contributor_id.0 != client_id
        {
            return Err(Error::Forbidden);
        }
        match request {
            Request::Status => Ok(json!({"target":self.destination.target,
                "configuration_revision":crate::workspace_config::RepositoryFiles::open(&self.destination.checkout_root)?.read()?.revision,
                "receipt":self.authority.as_ref().map(Authority::runtime_owner).transpose()?.map(|(receipt, _contributor)| receipt)})),
            Request::Upload {
                receipt,
                total,
                offset,
                content,
            } => self.upload(
                client_id,
                RuntimeChunk {
                    receipt,
                    total,
                    offset,
                    content: content.0,
                },
            ),
            Request::Commit { receipt } => self.commit(client_id, &receipt),
            Request::Call { command } => {
                let authority = self.authority.as_mut().ok_or(Error::Conflict)?;
                let (_receipt, contributor) = authority.runtime_owner()?;
                call(
                    authority,
                    &Principal {
                        contributor,
                        runtime: None,
                    },
                    command,
                )
            }
        }
    }

    fn upload(&mut self, client_id: &str, chunk: RuntimeChunk) -> Result<Value> {
        let RuntimeChunk {
            receipt,
            total,
            offset,
            content,
        } = chunk;
        if receipt.target != self.destination.target
            || total == 0
            || total > MAX_STATE_BYTES
            || content.len() > TRANSFER_CHUNK_BYTES.saturating_mul(2)
        {
            return Err(Error::Invalid);
        }
        if let Some(authority) = &self.authority {
            if authority.runtime_owner()?.0 != receipt {
                return Err(Error::Conflict);
            }
            return Ok(json!({"accepted":true}));
        }
        let bytes = STANDARD.decode(content).map_err(|_error| Error::Invalid)?;
        if bytes.is_empty()
            || bytes.len() > TRANSFER_CHUNK_BYTES
            || offset.saturating_add(bytes.len()) > total
        {
            return Err(Error::Invalid);
        }
        if offset == 0 {
            self.upload = Some(Upload {
                client_id: client_id.into(),
                receipt: receipt.clone(),
                total,
                bytes: Vec::new(),
            });
        }
        let upload = self.upload.as_mut().ok_or(Error::Conflict)?;
        if upload.client_id != client_id
            || upload.receipt != receipt
            || upload.total != total
            || upload.bytes.len() != offset
        {
            return Err(Error::Conflict);
        }
        upload.bytes.extend(bytes);
        Ok(json!({"received":upload.bytes.len()}))
    }

    fn commit(&mut self, client_id: &str, receipt: &RuntimeReceipt) -> Result<Value> {
        if let Some(authority) = &self.authority {
            let (previous, _contributor) = authority.runtime_owner()?;
            if &previous != receipt {
                return Err(Error::Conflict);
            }
            return Ok(serde_json::to_value(previous)?);
        }
        let upload = self.upload.as_ref().ok_or(Error::Conflict)?;
        if upload.client_id != client_id
            || &upload.receipt != receipt
            || upload.bytes.len() != upload.total
            || blake3::hash(&upload.bytes).to_hex().as_str() != receipt.package_hash
        {
            return Err(Error::Conflict);
        }
        let authority = Authority::import_runtime(
            self.storage.clone(),
            Arc::new(SystemClock),
            &self.destination,
            client_id,
            &upload.bytes,
        )?;
        let (accepted, _contributor) = authority.runtime_owner()?;
        if &accepted != receipt {
            return Err(Error::Conflict);
        }
        self.authority = Some(authority);
        self.upload = None;
        Ok(serde_json::to_value(accepted)?)
    }
}

fn call(authority: &mut Authority, principal: &Principal, command: Command) -> Result<Value> {
    if matches!(
        command,
        Command::Snapshot | Command::WorkspaceConfiguration | Command::CatchUp { .. }
    ) {
        authority.refresh_repository()?;
    }
    Ok(match command {
        Command::Snapshot => serde_json::to_value(authority.snapshot(principal)?)?,
        Command::WorkspaceConfiguration => {
            serde_json::to_value(authority.workspace_configuration()?)?
        }
        Command::Mutate(request) => serde_json::to_value(authority.execute(principal, *request)?)?,
        Command::RequestStatus(key) => {
            serde_json::to_value(authority.request_status(principal, &key)?)?
        }
        Command::CatchUp { after, limit } => {
            serde_json::to_value(authority.catch_up(principal, &after, limit)?)?
        }
        Command::CheckAccess(check) => {
            serde_json::to_value(authority.check_access(principal, &check)?)?
        }
        Command::ValidateControl(fence) => {
            serde_json::to_value(authority.validate_control(principal, &fence)?)?
        }
        Command::Presence => serde_json::to_value(authority.presence(principal)?)?,
        Command::PublishPresence(entry) => {
            authority.publish_presence(principal, entry)?;
            Value::Null
        }
        Command::RemovePresence(connection) => {
            authority.remove_presence(principal, &connection)?;
            Value::Null
        }
        Command::Versions
        | Command::SharingStatus
        | Command::JoinRequest
        | Command::InspectRequest(_)
        | Command::InspectInvitation(_)
        | Command::SharingScope
        | Command::Devices
        | Command::ImportSharing(_)
        | Command::ImportCleanup(_)
        | Command::Cleanup
        | Command::ConfigureDirectory(_)
        | Command::Host { .. }
        | Command::Join { .. }
        | Command::Resume
        | Command::Reconnect
        | Command::Scope(_)
        | Command::Revoke(_)
        | Command::Suspend
        | Command::Stop
        | Command::Discover
        | Command::PrepareAdoption(_)
        | Command::FinishAdoption
        | Command::PendingAdoption
        | Command::PrepareRuntimeTransfer { .. }
        | Command::RuntimeTransferStatus
        | Command::RuntimeTransferChunk(_)
        | Command::CompleteRuntimeTransfer(_) => return Err(Error::Forbidden),
    })
}

fn safe_error(_error: Error) -> io::Error {
    io::Error::other("Idle coordination owner is unavailable")
}
