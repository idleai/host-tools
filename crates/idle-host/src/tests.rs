use std::{io, time::Duration};

use serde_json::{Value, json};
use tokio::io::{AsyncWriteExt as _, DuplexStream, ReadHalf, WriteHalf};
use tokio_util::sync::CancellationToken;

use super::{
    protocol::{Frame, Kind, MAX_FRAME},
    serve,
};

struct Client {
    reader: ReadHalf<DuplexStream>,
    writer: WriteHalf<DuplexStream>,
    task: tokio::task::JoinHandle<io::Result<()>>,
}

impl Client {
    async fn start() -> io::Result<Self> {
        let (client, server) = tokio::io::duplex(64 * 1024);
        let (input, output) = tokio::io::split(server);
        let task =
            tokio::spawn(async move { serve(input, output, &CancellationToken::new()).await });
        let (reader, writer) = tokio::io::split(client);
        let mut client = Self {
            reader,
            writer,
            task,
        };
        let hello = client.receive().await?;
        equal(
            hello.kind,
            Kind::Hello,
            "host advertises compatibility before requests",
        )?;
        let value: Value = serde_json::from_slice(&hello.payload)?;
        equal(
            value.get("version"),
            Some(&json!(1)),
            "routing version is explicit",
        )?;
        equal(
            value.get("features"),
            Some(&json!([
                "repository.local",
                "runtime.workspace",
                "coordination.runtime-owner"
            ])),
            "optional repository and runtime reads are advertised before use",
        )?;
        Ok(client)
    }

    async fn send(&mut self, kind: Kind, channel: u32, payload: Vec<u8>) -> io::Result<()> {
        idle_host_io::asynchronous::write_frame(
            &mut self.writer,
            &Frame {
                kind,
                channel,
                payload,
            }
            .encode(),
            MAX_FRAME,
        )
        .await
    }

    async fn receive(&mut self) -> io::Result<Frame> {
        let bytes = tokio::time::timeout(
            Duration::from_secs(10),
            idle_host_io::asynchronous::read_frame(&mut self.reader, MAX_FRAME),
        )
        .await
        .map_err(io::Error::other)??
        .ok_or_else(|| io::Error::other("host closed early"))?;
        Frame::decode(&bytes)
    }

    async fn capture(&mut self, id: u32, root: &std::path::Path) -> io::Result<()> {
        self.send(Kind::Open, id, serde_json::to_vec(&json!({"workspace": root,
            "service": {"kind": "capture", "binding": {"workspace_path": root, "chain_dir": root.join("chain")}}}))?).await?;
        let ready = self.receive().await?;
        equal(
            (ready.kind, ready.channel),
            (Kind::Ready, id),
            "each binding has its own channel",
        )?;
        Ok(())
    }

    async fn stop(mut self) -> io::Result<()> {
        self.send(Kind::Shutdown, 0, Vec::new()).await?;
        self.writer.shutdown().await?;
        while idle_host_io::asynchronous::read_frame(&mut self.reader, MAX_FRAME)
            .await?
            .is_some()
        {}
        self.task.await.map_err(io::Error::other)?
    }
}

fn context(root: &std::path::Path) -> io::Result<Vec<u8>> {
    serde_json::to_vec(&json!({"id": u64::MAX, "body": {"GetEditorContext": {
        "workspace_path": root, "chain_dir": root.join("chain"),
    }}}))
    .map_err(io::Error::other)
}

#[tokio::test]
async fn malformed_service_closes_only_its_channel() -> io::Result<()> {
    let first = tempfile::tempdir()?;
    let second = tempfile::tempdir()?;
    let mut client = Client::start().await?;
    client.capture(1, first.path()).await?;
    client.capture(2, second.path()).await?;
    client.send(Kind::Data, 1, b"not JSON".to_vec()).await?;
    let closed = client.receive().await?;
    equal(
        (closed.kind, closed.channel),
        (Kind::Closed, 1),
        "invalid service data retires only its binding",
    )?;
    client.send(Kind::Data, 2, context(second.path())?).await?;
    let response = client.receive().await?;
    equal(
        (response.kind, response.channel),
        (Kind::Data, 2),
        "another workspace remains usable",
    )?;
    let response: Value = serde_json::from_slice(&response.payload)?;
    equal(
        response.get("id").and_then(Value::as_u64),
        Some(u64::MAX),
        "routing preserves every integer bit",
    )?;
    check(
        response
            .get("body")
            .and_then(|value| value.get("Ok"))
            .is_some(),
        "valid context read succeeds",
    )?;
    client.stop().await
}

#[tokio::test]
async fn capture_cannot_change_its_workspace() -> io::Result<()> {
    let first = tempfile::tempdir()?;
    let second = tempfile::tempdir()?;
    let mut client = Client::start().await?;
    client.capture(1, first.path()).await?;
    client.send(Kind::Data, 1, context(second.path())?).await?;
    let response: Value = serde_json::from_slice(&client.receive().await?.payload)?;
    check(
        response
            .get("body")
            .and_then(|value| value.get("Error"))
            .is_some(),
        "a request cannot replace its installed binding",
    )?;
    client.send(Kind::Close, 1, Vec::new()).await?;
    let closed = client.receive().await?;
    equal(
        closed.kind,
        Kind::Closed,
        "close acknowledges worker teardown",
    )?;
    client.capture(2, first.path()).await?;
    client.stop().await
}

#[tokio::test]
async fn invalid_binding_does_not_stop_other_services() -> io::Result<()> {
    let root = tempfile::tempdir()?;
    let mut client = Client::start().await?;
    client
        .send(
            Kind::Open,
            1,
            br#"{"workspace":"relative","service":{"kind":"history","binding":{}}}"#.to_vec(),
        )
        .await?;
    equal(
        client.receive().await?.kind,
        Kind::Closed,
        "invalid installation is rejected locally",
    )?;
    client.capture(2, root.path()).await?;
    client.stop().await
}

#[tokio::test]
async fn completion_does_not_discard_a_partial_next_frame() -> io::Result<()> {
    let root = tempfile::tempdir()?;
    let mut client = Client::start().await?;
    client.capture(1, root.path()).await?;
    client.capture(2, root.path()).await?;
    client.send(Kind::Close, 1, Vec::new()).await?;
    let frame = Frame {
        kind: Kind::Data,
        channel: 2,
        payload: context(root.path())?,
    }
    .encode();
    let length = u32::try_from(frame.len())
        .map_err(io::Error::other)?
        .to_le_bytes();
    let (first, remaining) = length.split_at(2);
    client.writer.write_all(first).await?;
    equal(
        client.receive().await?.kind,
        Kind::Closed,
        "worker finishes while the next request is fragmented",
    )?;
    client.writer.write_all(remaining).await?;
    client.writer.write_all(&frame).await?;
    let reply = client.receive().await?;
    equal(
        (reply.kind, reply.channel),
        (Kind::Data, 2),
        "the incomplete header remains intact",
    )?;
    client.stop().await
}

fn equal<T: Copy + PartialEq + std::fmt::Debug>(
    actual: T,
    expected: T,
    message: &str,
) -> io::Result<()> {
    if actual == expected {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "{message}: {actual:?} != {expected:?}"
        )))
    }
}

fn check(condition: bool, message: &str) -> io::Result<()> {
    if condition {
        Ok(())
    } else {
        Err(io::Error::other(message.to_owned()))
    }
}
