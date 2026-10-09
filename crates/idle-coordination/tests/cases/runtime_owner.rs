use std::{fs, io, sync::Arc};

use idle_coordination::{
    Error,
    authority::{
        Authority,
        runtime_transfer::{RuntimeChunk, RuntimeReceipt, RuntimeTarget},
    },
    persistence::FilePersistence,
    service::{read_frame, write_frame},
};
use serde_json::{Value, json};
use tokio::io::DuplexStream;
use tokio_util::sync::CancellationToken;

use super::support::{self, TestClock, bootstrap, principal};

struct Fixture {
    _root: tempfile::TempDir,
    configuration: Value,
    receipt: RuntimeReceipt,
    chunk: RuntimeChunk,
    directory: std::path::PathBuf,
}

fn fixture() -> Result<Fixture, Box<dyn std::error::Error + Send + Sync>> {
    let root = tempfile::tempdir()?;
    let source = root.path().join("source");
    let target = root.path().join("target");
    let directory = root.path().join("daemon-private");
    fs::create_dir(&source)?;
    fs::create_dir_all(target.join(".idle/workspace"))?;
    let mut authority = Authority::open_repository(
        Arc::new(FilePersistence::open(root.path().join("source-private"))?),
        Arc::new(TestClock::default()),
        bootstrap(),
        &source,
    )?;
    for entry in fs::read_dir(source.join(".idle/workspace"))? {
        let entry = entry?;
        if entry.file_type()?.is_file() {
            let _copied = fs::copy(
                entry.path(),
                target.join(".idle/workspace").join(entry.file_name()),
            )?;
        }
    }
    let target_binding = RuntimeTarget {
        host_id: "daemon".into(),
        checkout_id: "checkout".into(),
    };
    let revision = authority.workspace_configuration()?.revision.clone();
    let receipt = authority.prepare_runtime_transfer(
        &principal("owner"),
        target_binding.clone(),
        &revision,
    )?;
    let chunk = authority.runtime_transfer_chunk(&principal("owner"), 0)?;
    Ok(Fixture {
        configuration: json!({"version":1,"state_directory":directory,"destination":{
            "target":target_binding,"workspace_id":"workspace","repository_id":"repository","chain_id":"logical-chain","checkout_root":target
        }}),
        _root: root,
        receipt,
        chunk,
        directory,
    })
}

async fn start(
    configuration: &Value,
) -> Result<
    (DuplexStream, tokio::task::JoinHandle<io::Result<()>>),
    Box<dyn std::error::Error + Send + Sync>,
> {
    let (mut client, server) = tokio::io::duplex(64 * 1024);
    let (input, output) = tokio::io::split(server);
    let task = tokio::spawn(async move {
        idle_coordination::runtime::serve_authority(input, output, &CancellationToken::new()).await
    });
    equal!(
        read_frame::<Value>(&mut client).await?,
        Some(json!({"version":1})),
        "helper negotiates its protocol before loading storage"
    )?;
    write_frame(&mut client, configuration).await?;
    equal!(
        read_frame::<Value>(&mut client).await?,
        Some(json!({"ready":true})),
        "helper acquires private ownership before readiness"
    )?;
    Ok((client, task))
}

async fn call(
    client: &mut DuplexStream,
    contributor: &str,
    request: Value,
) -> idle_coordination::Result<Value> {
    write_frame(client, &json!({"client_id":contributor,"request":request})).await?;
    read_frame(client).await?.ok_or(Error::Transport)
}

#[tokio::test]
async fn daemon_helper_holds_one_owner_and_restores_after_editor_detach() -> support::TestResult {
    let fixture = fixture()?;
    let (mut client, task) = start(&fixture.configuration).await?;
    ensure!(
        matches!(FilePersistence::open(&fixture.directory), Err(Error::Busy)),
        "a second helper cannot acquire the same workspace"
    )?;
    let upload = json!({"kind":"upload","receipt":fixture.receipt,"offset":fixture.chunk.offset,"total":fixture.chunk.total,"content":fixture.chunk.content});
    let sent = call(&mut client, "owner", upload).await?;
    ensure!(
        sent.get("Ok").is_some(),
        "the complete bounded package is received"
    )?;
    let accepted = call(
        &mut client,
        "owner",
        json!({"kind":"commit","receipt":fixture.receipt}),
    )
    .await?;
    equal!(
        &accepted,
        &json!({"Ok":fixture.receipt}),
        "the durable receipt matches the source"
    )?;
    equal!(
        call(&mut client, "guest", json!({"kind":"status"})).await?,
        json!({"Err":"forbidden"}),
        "another runtime client cannot read owner metadata"
    )?;
    equal!(
        call(
            &mut client,
            "owner",
            json!({"kind":"call","command":{"kind":"stop"}})
        )
        .await?,
        json!({"Err":"forbidden"}),
        "runtime ownership does not grant history lifecycle access"
    )?;
    drop(client);
    task.await??;
    let (mut client, task) = start(&fixture.configuration).await?;
    equal!(
        call(
            &mut client,
            "owner",
            json!({"kind":"commit","receipt":fixture.receipt})
        )
        .await?,
        accepted,
        "lost response retry requires no new upload after restart"
    )?;
    let snapshot = call(
        &mut client,
        "owner",
        json!({"kind":"call","command":{"kind":"snapshot"}}),
    )
    .await?;
    equal!(
        snapshot.pointer("/Ok/workspace/value/id"),
        Some(&json!("workspace")),
        "the restarted helper serves the original workspace"
    )?;
    drop(client);
    task.await??;
    Ok(())
}

#[tokio::test]
async fn incomplete_or_changed_uploads_cannot_commit_an_owner() -> support::TestResult {
    let fixture = fixture()?;
    let (mut client, task) = start(&fixture.configuration).await?;
    let commit = json!({"kind":"commit","receipt":fixture.receipt});
    equal!(
        call(&mut client, "owner", commit.clone()).await?,
        json!({"Err":"conflict"}),
        "a commit needs the complete original payload"
    )?;
    let mut upload = json!({"kind":"upload","receipt":fixture.receipt,"offset":0,"total":fixture.chunk.total,"content":fixture.chunk.content});
    *upload.get_mut("offset").ok_or(Error::Invalid)? = json!(1);
    ensure!(
        call(&mut client, "owner", upload.clone())
            .await?
            .get("Err")
            .is_some(),
        "out-of-order upload must be rejected"
    )?;
    *upload.get_mut("offset").ok_or(Error::Invalid)? = json!(0);
    let _received = call(&mut client, "owner", upload).await?;
    equal!(
        call(&mut client, "guest", commit.clone()).await?,
        json!({"Err":"conflict"}),
        "one client cannot complete another client's upload"
    )?;
    let accepted = call(&mut client, "owner", commit).await?;
    ensure!(
        accepted.get("Ok").is_some(),
        "the original client can still finish"
    )?;
    drop(client);
    task.await??;
    Ok(())
}
