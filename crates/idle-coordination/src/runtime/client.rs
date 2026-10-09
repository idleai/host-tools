use std::{
    collections::BTreeMap,
    io,
    sync::{Arc, Mutex},
    time::Duration,
};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    sync::mpsc,
};
use tokio_util::sync::CancellationToken;

use crate::{
    Error, Result,
    clock::{Clock as _, SystemClock},
    dev_tunnels::DevTunnels,
    invitation::Secret,
    service::ServiceResponse,
    transport::{BoxStream, ClientTransport, bounded},
};

use super::{RuntimeInvitation, credentials::NoManagement, framing};

/// Explicit runtime connection approved for a local workspace and contributor.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    /// Private daemon-issued invitation; never render or log this value.
    pub invitation: Secret,
    /// Selected local workspace must match the invitation.
    pub workspace_id: String,
    /// Selected repository must match the invitation.
    pub repository_id: String,
    /// Selected logical chain must match the invitation.
    pub chain_id: String,
    /// Authenticated local contributor to whom the invitation was issued.
    pub client_id: String,
}

#[derive(Debug, Deserialize)]
#[serde(
    tag = "kind",
    content = "data",
    rename_all = "snake_case",
    deny_unknown_fields
)]
enum Message {
    Call(Call),
    Cancel(String),
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Call {
    version: u32,
    id: String,
    timeout_ms: u32,
    command: Operation,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Operation {
    Status,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
struct WorkspaceBinding {
    workspace_id: String,
    repository_id: String,
    checkout_id: String,
    chain_id: String,
    checkout_root: String,
    chain_directory: String,
}

impl WorkspaceBinding {
    fn matches(&self, invitation: &RuntimeInvitation) -> bool {
        self.workspace_id == invitation.workspace_id
            && self.repository_id == invitation.repository_id
            && self.checkout_id == invitation.checkout_id
            && self.chain_id == invitation.chain_id
    }
}

/// Serve serialized runtime reads with cancellation and bounded reconnect attempts.
/// A response contains only validated daemon status, never invitation credentials.
/// # Errors
/// Returns invalid framing, mismatched bindings or a broken private pipe.
pub async fn serve_client(
    binding: Binding,
    mut input: impl AsyncRead + Unpin + Send,
    mut output: impl AsyncWrite + Unpin + Send,
    cancel: &CancellationToken,
) -> io::Result<()> {
    let mut client = Client::new(&binding).map_err(|_error| framing::invalid())?;
    let lifetime = cancel.child_token();
    let controls = Mutex::new(BTreeMap::<String, CancellationToken>::new());
    let (calls, mut incoming) = mpsc::channel(8);
    let reading = async {
        let result = async {
            loop {
                let message = tokio::select! {
                    () = lifetime.cancelled() => return Ok(()),
                    message = framing::read::<Message>(&mut input) => message?,
                };
                let Some(message) = message else {
                    return Ok(());
                };
                match message {
                    Message::Call(call) => {
                        if call.version != 1
                            || call.id.is_empty()
                            || call.id.len() > 256
                            || !(1..=60_000).contains(&call.timeout_ms)
                        {
                            return Err(framing::invalid());
                        }
                        let token = lifetime.child_token();
                        {
                            let mut pending =
                                controls.lock().map_err(|_error| framing::invalid())?;
                            if pending.len() >= 8 || pending.contains_key(&call.id) {
                                return Err(framing::invalid());
                            }
                            let _previous = pending.insert(call.id.clone(), token.clone());
                        }
                        calls
                            .try_send((call, token))
                            .map_err(|_error| framing::invalid())?;
                    }
                    Message::Cancel(id) => {
                        if let Some(token) = controls
                            .lock()
                            .map_err(|_error| framing::invalid())?
                            .get(&id)
                        {
                            token.cancel();
                        }
                    }
                }
            }
        }
        .await;
        lifetime.cancel();
        result
    };
    let serving = async {
        let result = async {
            loop {
                let next = tokio::select! {
                    () = lifetime.cancelled() => return Ok(()),
                    next = incoming.recv() => next,
                };
                let Some((call, token)) = next else {
                    return Ok(());
                };
                let result = Box::pin(bounded(
                    &token,
                    Duration::from_millis(u64::from(call.timeout_ms)),
                    async {
                        match call.command {
                            Operation::Status => client.status(&token).await,
                        }
                    },
                ))
                .await;
                if result.is_err() {
                    client.close().await;
                }
                let _removed = controls
                    .lock()
                    .map_err(|_error| framing::invalid())?
                    .remove(&call.id);
                let response = ServiceResponse {
                    version: 1,
                    id: call.id,
                    result,
                };
                framing::write(&mut output, &response).await?;
            }
        }
        .await;
        lifetime.cancel();
        client.close().await;
        result
    };
    let (read, served) = tokio::join!(reading, serving);
    read?;
    served
}

struct Client {
    invitation: RuntimeInvitation,
    transport: Option<Box<dyn ClientTransport>>,
    stream: Option<BoxStream>,
    next_id: u64,
}

impl Client {
    fn new(binding: &Binding) -> Result<Self> {
        let invitation = RuntimeInvitation::parse(&binding.invitation.0, SystemClock.now_ms()?)?;
        if invitation.workspace_id != binding.workspace_id
            || invitation.repository_id != binding.repository_id
            || invitation.chain_id != binding.chain_id
            || invitation.client_id != binding.client_id
        {
            return Err(Error::Forbidden);
        }
        Ok(Self {
            invitation,
            transport: None,
            stream: None,
            next_id: 0,
        })
    }

    async fn connect(&mut self, cancel: &CancellationToken) -> Result<()> {
        let adapter = DevTunnels::runtime(
            Arc::new(NoManagement),
            Arc::new(NoManagement),
            Arc::new(SystemClock),
        )?;
        let mut transport = adapter
            .connect_runtime(&self.invitation.relay, cancel)
            .await?;
        let stream = transport.take_stream()?;
        self.transport = Some(transport);
        self.stream = Some(stream);
        let invitation = &self.invitation;
        let response = self
            .exchange(
                json!({"kind":"authenticate", "version":1, "grantId":invitation.grant_id,
            "token":invitation.grant_token.0}),
            )
            .await?;
        if response.get("kind").and_then(Value::as_str) != Some("authenticated")
            || response.get("hostId").and_then(Value::as_str)
                != Some(self.invitation.host_id.as_str())
        {
            return Err(Error::Forbidden);
        }
        let authenticated: WorkspaceBinding =
            serde_json::from_value(response.get("binding").cloned().ok_or(Error::Invalid)?)
                .map_err(|_error| Error::Invalid)?;
        if !authenticated.matches(&self.invitation) {
            return Err(Error::Forbidden);
        }
        // Initialize is still the native app-server handshake after grant authentication.
        let _initialized = self
            .rpc(
                "initialize",
                json!({
                    "clientInfo":{"name":"idle-runtime","version":env!("CARGO_PKG_VERSION")},
                    "capabilities":{"experimentalApi":true}
                }),
            )
            .await?;
        framing::write(
            self.stream.as_mut().ok_or(Error::Transport)?,
            &json!({"method":"initialized"}),
        )
        .await?;
        let _attached = self
            .rpc(
                "idle/workspace/attach",
                json!({"protocolVersion":1,"binding":authenticated}),
            )
            .await?;
        Ok(())
    }

    async fn status(&mut self, cancel: &CancellationToken) -> Result<Value> {
        if self.invitation.expires_at <= SystemClock.now_ms()? {
            return Err(Error::Expired);
        }
        if self.stream.is_none() {
            self.connect(cancel).await?;
        }
        let response = self
            .rpc("idle/runtime/status/read", json!({"protocolVersion":1}))
            .await?;
        let status = response.get("status").ok_or(Error::Invalid)?;
        if status.get("protocolVersion").and_then(Value::as_u64) != Some(1)
            || status.get("hostId").and_then(Value::as_str)
                != Some(self.invitation.host_id.as_str())
        {
            return Err(Error::Version);
        }
        let workspaces = status
            .get("workspaces")
            .and_then(Value::as_array)
            .ok_or(Error::Invalid)?;
        if workspaces.len() != 1 {
            return Err(Error::Forbidden);
        }
        let binding: WorkspaceBinding = serde_json::from_value(
            workspaces
                .first()
                .and_then(|entry| entry.get("binding"))
                .cloned()
                .ok_or(Error::Invalid)?,
        )
        .map_err(|_error| Error::Invalid)?;
        if !binding.matches(&self.invitation) {
            return Err(Error::Forbidden);
        }
        Ok(response)
    }

    async fn rpc(&mut self, method: &str, params: Value) -> Result<Value> {
        self.next_id = self.next_id.checked_add(1).ok_or(Error::Invalid)?;
        let id = self.next_id;
        let response = self
            .exchange(json!({"id":id,"method":method,"params":params}))
            .await?;
        if response.get("id").and_then(Value::as_u64) != Some(id) {
            return Err(Error::Invalid);
        }
        if response.get("error").is_some() {
            return Err(Error::Forbidden);
        }
        response.get("result").cloned().ok_or(Error::Invalid)
    }

    async fn exchange(&mut self, message: Value) -> Result<Value> {
        let stream = self.stream.as_mut().ok_or(Error::Transport)?;
        framing::write(stream, &message).await?;
        framing::read(stream).await?.ok_or(Error::Transport)
    }

    async fn close(&mut self) {
        self.stream = None;
        if let Some(mut transport) = self.transport.take() {
            let _closed = transport.close().await;
        }
    }
}
