use std::{path::Path, process::Stdio, sync::Arc, time::Duration};

use idle_coordination::{
    Error, Result,
    clock::{Clock as _, SystemClock},
    engine::ScopeChoice,
    invitation::{JoinRequest, RequestKind, encode},
    service::{Client, Command, Service, native::Configuration, read_frame, serve},
};
use idle_protocol::v1::{
    Change, WriteCondition,
    api::{ApiResult, Response},
    configuration::{ConfigurationDocument, ConfigurationValue, ConfigurationWrite},
    identity::{Revision, Timestamp},
    standalone::{Mutation, MutationResult, RepositorySnapshot},
};
use serde_json::Value;
use tokio::{
    io::AsyncWriteExt as _,
    process::{Child, ChildStdin, ChildStdout},
};
use tokio_util::sync::CancellationToken;

use super::support::{
    self, Memory, NOW, TestClock, authority, principal,
    relay::{Credential, Relay},
    seed, until,
};

fn launch(path: &Path) -> Result<(Child, Client<ChildStdout, ChildStdin>)> {
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_idle-coordination"))
        .args(["--config", path.to_str().ok_or(Error::Invalid)?])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;
    let reader = child.stdout.take().ok_or(Error::Invalid)?;
    let writer = child.stdin.take().ok_or(Error::Invalid)?;
    Ok((child, Client::new(reader, writer)))
}

#[tokio::test]
async fn automatic_resume_keeps_a_completed_stop_disabled() -> support::TestResult {
    let root = tempfile::tempdir()?;
    let guest_root = tempfile::tempdir()?;
    let engine = support::engine(root.path());
    seed(&engine.chain, 1, 1, b"retained history")?;
    let storage = support::storage(root.path())?;
    let relay = Relay::new(Arc::new(TestClock::default()));
    let mut peers = relay
        .coordinator(
            engine.clone(),
            storage.clone(),
            Arc::new(Credential::default()),
        )
        .await?;
    let guest = support::engine(guest_root.path()).identity().await?;
    let _invitation = peers
        .host_history(
            &encode(&JoinRequest {
                version: 1,
                kind: RequestKind::Request,
                device: guest,
            })?,
            ScopeChoice::FromNow,
            &CancellationToken::new(),
        )
        .await?;
    let scope = engine.scope().await?;
    peers.stop().await?;
    drop(peers);
    drop(storage);
    let config = Configuration {
        state_directory: root.path().join("private"),
        chain_directory: engine.chain.clone(),
        device_directory: engine.device_directory.clone(),
        workspace: support::workspace(),
        contributor: principal("owner").contributor,
        runtime: None,
        credential_variable: None,
        discovery_repository: None,
        resume_sharing: true,
    };
    let mut service = config.open().await?;
    ensure!(
        !service.peers.status().enabled,
        "a completed Stop cannot automatically resume"
    )?;
    equal!(
        engine.scope().await?,
        scope,
        "Stop and restart must preserve the engine boundary"
    )?;
    let _snapshot = service
        .call(Command::Snapshot, &CancellationToken::new())
        .await?;
    service.suspend().await?;
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn blocked_request_write_observes_cancellation_and_invalidates_client() -> support::TestResult
{
    let (client_stream, _unread_server) = tokio::io::duplex(1);
    let (reader, writer) = tokio::io::split(client_stream);
    let mut client = Client::new(reader, writer);
    let cancel = CancellationToken::new();
    let interrupt = async {
        tokio::time::sleep(Duration::from_millis(10)).await;
        cancel.cancel();
    };
    let pending = tokio::time::timeout(
        Duration::from_secs(1),
        client.call(Command::Versions, 60_000, &cancel),
    );
    let (result, ()) = tokio::join!(pending, interrupt);
    equal!(
        result?,
        Err(Error::Cancelled),
        "cancellation must interrupt a partially written frame"
    )?;
    equal!(
        client
            .call(Command::Versions, 1000, &CancellationToken::new())
            .await,
        Err(Error::Transport),
        "partial writes must invalidate the logical connection"
    )?;
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn blocked_request_write_observes_its_timeout() -> support::TestResult {
    let (client_stream, _unread_server) = tokio::io::duplex(1);
    let (reader, writer) = tokio::io::split(client_stream);
    let mut client = Client::new(reader, writer);
    let result = tokio::time::timeout(
        Duration::from_secs(1),
        client.call(Command::Versions, 10, &CancellationToken::new()),
    )
    .await?;
    equal!(
        result,
        Err(Error::Timeout),
        "request writes must use the call's timeout"
    )?;
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn blocked_cancellation_notice_uses_the_cleanup_deadline() -> support::TestResult {
    let message =
        idle_coordination::service::Message::Call(idle_coordination::service::ServiceRequest {
            version: 1,
            id: "1".into(),
            timeout_ms: 60_000,
            command: Command::Versions,
        });
    let capacity = serde_json::to_vec(&message)?.len().saturating_add(4);
    let (client_stream, _unread_server) = tokio::io::duplex(capacity);
    let (reader, writer) = tokio::io::split(client_stream);
    let mut client = Client::new(reader, writer);
    let cancel = CancellationToken::new();
    let interrupt = async {
        tokio::time::sleep(Duration::from_millis(10)).await;
        cancel.cancel();
    };
    let pending = tokio::time::timeout(
        Duration::from_secs(16),
        client.call(Command::Versions, 60_000, &cancel),
    );
    let (result, ()) = tokio::join!(pending, interrupt);
    equal!(
        result?,
        Err(Error::Timeout),
        "the cleanup deadline must include writing Cancel"
    )?;
    equal!(
        client
            .call(Command::Versions, 1000, &CancellationToken::new())
            .await,
        Err(Error::Transport),
        "an unfinished cancellation notice must invalidate framing"
    )?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn standalone_executable_serves_and_recovers_without_node_or_application_hosts()
-> support::TestResult {
    let root = tempfile::tempdir()?;
    let engine = support::engine(root.path());
    seed(&engine.chain, 1, 1, b"native history")?;
    let owner = principal("owner");
    let config = Configuration {
        state_directory: root.path().join("private"),
        chain_directory: engine.chain.clone(),
        device_directory: engine.device_directory.clone(),
        workspace: support::workspace(),
        contributor: owner.contributor.clone(),
        runtime: None,
        credential_variable: Some("UNUSED_TEST_OWNER_CREDENTIAL".into()),
        discovery_repository: None,
        resume_sharing: false,
    };
    let path = root.path().join("service.json");
    std::fs::write(&path, serde_json::to_vec(&config)?)?;
    let (mut child, mut client) = launch(&path)?;
    let cancel = CancellationToken::new();
    let versions = client.call(Command::Versions, 5000, &cancel).await?;
    equal!(
        versions.get("engine_peer"),
        Some(&Value::from(5)),
        "native consumers must be able to negotiate the engine boundary"
    )?;
    let first: RepositorySnapshot =
        serde_json::from_value(client.call(Command::Snapshot, 5000, &cancel).await?)?;
    let mut mutation = support::request(
        &owner,
        "native-settings",
        Mutation::Configuration(ConfigurationWrite {
            document: ConfigurationDocument::Settings,
            change: Change {
                expected: WriteCondition::Absent,
                value: ConfigurationValue {
                    schema_version: 1,
                    json: r#"{"native":true,"unknown":42}"#.into(),
                },
            },
        }),
    );
    mutation.context.expires_at = Timestamp(SystemClock.now_ms()?.saturating_add(60_000));
    let committed: Response<MutationResult> = serde_json::from_value(
        client
            .call(Command::Mutate(Box::new(mutation.clone())), 5000, &cancel)
            .await?,
    )?;
    ensure!(
        matches!(committed.result, ApiResult::Success(_)),
        "the executable must commit typed native requests"
    )?;
    // Kill after a confirmed commit: durable retries must survive abrupt process loss too.
    child.start_kill()?;
    let _status = child.wait().await?;
    drop(client);
    let (mut restarted, mut client) = launch(&path)?;
    let after: RepositorySnapshot =
        serde_json::from_value(client.call(Command::Snapshot, 5000, &cancel).await?)?;
    equal!(
        after.workspace.value.chain,
        first.workspace.value.chain,
        "process restart must retain logical chain identity"
    )?;
    equal!(
        after
            .settings
            .expect("confirmed settings must survive")
            .revision,
        Revision(1),
        "a restart must not manufacture a new revision"
    )?;
    let retry: Response<MutationResult> = serde_json::from_value(
        client
            .call(Command::Mutate(Box::new(mutation)), 5000, &cancel)
            .await?,
    )?;
    equal!(
        retry,
        committed,
        "the service must recover the exact original retry result"
    )?;
    drop(client);
    let exit = tokio::time::timeout(Duration::from_secs(10), restarted.wait())
        .await
        .map_err(|_error| Error::Timeout)??;
    ensure!(
        exit.success(),
        "EOF must drain the service and release the directory lock"
    )?;
    Ok(())
}

#[tokio::test]
async fn framed_sideband_cancellation_drains_pending_host_and_keeps_connection_usable()
-> support::TestResult {
    let root = tempfile::tempdir()?;
    let guest_root = tempfile::tempdir()?;
    let engine = support::engine(root.path());
    seed(&engine.chain, 1, 1, b"local")?;
    let guest = support::engine(guest_root.path()).identity().await?;
    let clock = Arc::new(TestClock::default());
    let relay = Relay::new(clock.clone());
    relay
        .block_host
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let storage = Arc::new(Memory::default());
    let peers = relay
        .coordinator(
            engine.clone(),
            storage.clone(),
            Arc::new(Credential::default()),
        )
        .await?;
    let mut service = Service {
        authority: authority(storage, clock.clone())?,
        peers,
        principal: principal("owner"),
        engine,
        clock,
        directory: None,
        adoption: None,
    };
    let (server_stream, client_stream) = tokio::io::duplex(64 * 1024);
    let (server_read, server_write) = tokio::io::split(server_stream);
    let (client_read, client_write) = tokio::io::split(client_stream);
    let serving = tokio::spawn(async move {
        serve(
            &mut service,
            server_read,
            server_write,
            &CancellationToken::new(),
        )
        .await
    });
    let mut client = Client::new(client_read, client_write);
    let cancel = CancellationToken::new();
    let interrupted = cancel.clone();
    let host = Command::Host {
        request: idle_coordination::invitation::Secret(encode(&JoinRequest {
            version: 1,
            kind: RequestKind::Request,
            device: guest,
        })?),
        scope: ScopeChoice::All,
    };
    let pending = client.call(host, 30_000, &cancel);
    let interrupt = async {
        until(|| {
            relay
                .host_attempts
                .load(std::sync::atomic::Ordering::SeqCst)
                > 0
        })
        .await;
        interrupted.cancel();
    };
    let (result, ()) = tokio::join!(pending, interrupt);
    equal!(
        result,
        Err(Error::Cancelled),
        "sideband cancellation must reach pending adapter work"
    )?;
    let status = client
        .call(Command::SharingStatus, 5000, &CancellationToken::new())
        .await?;
    equal!(
        status.get("enabled"),
        Some(&Value::Bool(false)),
        "cancelled approval must not leave a live generation"
    )?;
    let _snapshot = client
        .call(Command::Snapshot, 5000, &CancellationToken::new())
        .await?;
    drop(client);
    serving.await.map_err(|_error| Error::Transport)??;
    Ok(())
}

#[tokio::test]
async fn local_framing_rejects_oversize_and_truncated_input_before_dispatch() {
    let header = u32::MAX.to_be_bytes();
    assert_eq!(
        read_frame::<Value>(&mut header.as_slice()).await,
        Err(Error::Invalid),
        "framing must cap input before payload allocation"
    );
    let truncated = [0_u8, 0, 0, 5, b'{'];
    assert!(
        read_frame::<Value>(&mut truncated.as_slice())
            .await
            .is_err(),
        "truncated frames cannot become executable requests"
    );
    assert_eq!(
        read_frame::<Value>(&mut b"".as_slice()).await,
        Ok(None),
        "clean EOF is distinct from truncated input"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rust_coordinator_replicates_with_existing_typescript_peer_bridge() -> support::TestResult {
    let root = tempfile::tempdir()?;
    let guest_root = tempfile::tempdir()?;
    let engine = support::engine(root.path());
    let guest = support::engine(guest_root.path());
    seed(&engine.chain, 1, 1, &vec![7; 262_144])?;
    seed(&guest.chain, 2, 1, b"original TypeScript-side record")?;
    let guest_device = guest.identity().await?;
    let relay = Relay::new(Arc::new(TestClock::default()));
    let mut coordinator = relay
        .coordinator(
            engine.clone(),
            Arc::new(Memory::default()),
            Arc::new(Credential::default()),
        )
        .await?;
    let request = encode(&JoinRequest {
        version: 1,
        kind: RequestKind::Request,
        device: guest_device,
    })?;
    let encoded = coordinator
        .host_history(&request, ScopeChoice::All, &CancellationToken::new())
        .await?;
    let invitation =
        idle_coordination::invitation::Invitation::parse(&encoded, &guest.identity().await?, NOW)?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let address = listener.local_addr()?;
    let sender = relay
        .hosts
        .lock()
        .map_err(|_error| Error::Storage)?
        .get(&invitation.endpoint.tunnel_id)
        .cloned()
        .ok_or(Error::Transport)?;
    let forwarding = tokio::spawn(async move {
        let (mut tcp, _address) = listener.accept().await?;
        let (mut stream, incoming) = tokio::io::duplex(8192);
        sender
            .send(incoming)
            .await
            .map_err(|_error| std::io::Error::other("host closed"))?;
        tokio::io::copy_bidirectional(&mut tcp, &mut stream).await
    });
    let repository = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let binary = repository.join("../editchain/target/debug/editchain-peer");
    ensure!(
        binary.exists(),
        "lint's engine integration tools step must build the existing peer worker"
    )?;
    let mut child = tokio::process::Command::new("node")
        .arg(repository.join("scripts/coordination-peer.cjs"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()?;
    let config = serde_json::json!({ "binary": binary, "chain": guest.chain, "device": guest.device_directory,
        "invitation": encoded, "now": NOW, "port": address.port() });
    let mut input = child.stdin.take().ok_or(Error::Invalid)?;
    input.write_all(&serde_json::to_vec(&config)?).await?;
    input.shutdown().await?;
    drop(input);
    let output = tokio::time::timeout(Duration::from_secs(30), child.wait_with_output())
        .await
        .map_err(|_error| Error::Timeout)??;
    ensure!(
        output.status.success(),
        "existing TypeScript bridge failed: {}",
        String::from_utf8_lossy(&output.stderr)
    )?;
    equal!(
        support::record_count(&engine)?,
        2,
        "Rust must durably receive the TypeScript peer's record"
    )?;
    equal!(
        support::record_count(&guest)?,
        2,
        "TypeScript must durably receive the Rust peer's record"
    )?;
    let progress: Value = serde_json::from_slice(&output.stdout)?;
    equal!(
        progress.get("received_blobs"),
        Some(&Value::from(1)),
        "TypeScript bridge must hydrate the large remote blob"
    )?;
    coordinator.stop().await?;
    let _copied = tokio::time::timeout(Duration::from_secs(5), forwarding)
        .await
        .map_err(|_error| Error::Timeout)?
        .map_err(|_error| Error::Transport)??;
    Ok(())
}
