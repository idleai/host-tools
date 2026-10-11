use std::{
    collections::{BTreeMap, VecDeque},
    sync::{Arc, Mutex},
};

use serde_json::{Value, json};
use tokio::{io::WriteHalf, sync::mpsc, task::JoinHandle};
use tokio_util::sync::CancellationToken;

use super::framing;
use crate::{Error, Result, transport::BoxStream};

const MAX_NOTIFICATIONS: usize = 64;
const MAX_BYTES: usize = 256 * 1024;

#[derive(Default)]
struct Mailbox {
    notifications: VecDeque<(Value, usize)>,
    notification_bytes: usize,
    approvals: BTreeMap<String, (Value, usize)>,
    approval_bytes: usize,
    dropped: u64,
}

impl Mailbox {
    fn record(&mut self, message: Value) -> Result<()> {
        let size = serde_json::to_vec(&message)?.len();
        if let Some(id) = message.get("id") {
            if message.get("method").and_then(Value::as_str)
                != Some("item/commandExecution/requestApproval")
                || self.approvals.len() >= 32
                || self.approval_bytes.saturating_add(size) > MAX_BYTES
            {
                return Err(Error::Invalid);
            }
            let key = request_key(id)?;
            if self.approvals.contains_key(&key) {
                return Err(Error::Invalid);
            }
            self.approval_bytes = self.approval_bytes.saturating_add(size);
            let _previous = self.approvals.insert(key, (message, size));
            return Ok(());
        }
        if message.get("method").and_then(Value::as_str) == Some("serverRequest/resolved")
            && let Some(id) = message.pointer("/params/requestId")
        {
            self.remove_approval(id)?;
        }
        if size > MAX_BYTES {
            self.dropped = self.dropped.saturating_add(1);
            return Ok(());
        }
        while self.notifications.len() >= MAX_NOTIFICATIONS
            || self.notification_bytes.saturating_add(size) > MAX_BYTES
        {
            if let Some((_, removed)) = self.notifications.pop_front() {
                self.notification_bytes = self.notification_bytes.saturating_sub(removed);
                self.dropped = self.dropped.saturating_add(1);
            }
        }
        self.notification_bytes = self.notification_bytes.saturating_add(size);
        self.notifications.push_back((message, size));
        Ok(())
    }

    fn remove_approval(&mut self, id: &Value) -> Result<()> {
        if let Some((_, size)) = self.approvals.remove(&request_key(id)?) {
            self.approval_bytes = self.approval_bytes.saturating_sub(size);
        }
        Ok(())
    }

    fn drain(&mut self) -> Value {
        let notifications: Vec<_> = self
            .notifications
            .drain(..)
            .map(|(value, _)| value)
            .collect();
        self.notification_bytes = 0;
        let approvals: Vec<_> = self
            .approvals
            .values()
            .map(|(value, _)| value.clone())
            .collect();
        json!({"notifications":notifications,"approvals":approvals,"droppedNotifications":std::mem::take(&mut self.dropped)})
    }
}

fn request_key(id: &Value) -> Result<String> {
    if !id.is_i64()
        && !id
            .as_str()
            .is_some_and(|text| !text.is_empty() && text.len() <= 256)
    {
        return Err(Error::Invalid);
    }
    Ok(serde_json::to_string(id)?)
}

pub(super) struct RemoteStream {
    writer: WriteHalf<BoxStream>,
    responses: mpsc::Receiver<Result<Value>>,
    mailbox: Arc<Mutex<Mailbox>>,
    cancel: CancellationToken,
    task: Option<JoinHandle<()>>,
}

impl RemoteStream {
    pub(super) fn new(stream: BoxStream) -> Self {
        let (mut reader, writer) = tokio::io::split(stream);
        let (sender, responses) = mpsc::channel(8);
        let mailbox = Arc::new(Mutex::new(Mailbox::default()));
        let incoming = mailbox.clone();
        let cancel = CancellationToken::new();
        let reading = cancel.clone();
        let task = tokio::spawn(async move {
            loop {
                let message = tokio::select! {
                    () = reading.cancelled() => break,
                    message = framing::read::<Value>(&mut reader) => message,
                };
                let Ok(Some(message)) = message else {
                    break;
                };
                if message.get("method").is_some() {
                    let recorded = incoming
                        .lock()
                        .map_err(|_error| Error::Storage)
                        .and_then(|mut mailbox| mailbox.record(message));
                    if recorded.is_err() {
                        break;
                    }
                } else if sender.try_send(Ok(message)).is_err() {
                    break;
                }
            }
            let _sent = sender.try_send(Err(Error::Transport));
            reading.cancel();
        });
        Self {
            writer,
            responses,
            mailbox,
            cancel,
            task: Some(task),
        }
    }

    pub(super) async fn send(&mut self, message: &Value) -> Result<()> {
        if self.cancel.is_cancelled() {
            return Err(Error::Transport);
        }
        framing::write(&mut self.writer, message).await?;
        Ok(())
    }

    pub(super) async fn exchange(&mut self, message: &Value) -> Result<Value> {
        self.send(message).await?;
        self.responses.recv().await.ok_or(Error::Transport)?
    }

    pub(super) fn events(&self) -> Result<Value> {
        if self.cancel.is_cancelled() {
            return Err(Error::Transport);
        }
        Ok(self
            .mailbox
            .lock()
            .map_err(|_error| Error::Storage)?
            .drain())
    }

    pub(super) async fn approve(&mut self, id: Value, decision: &str) -> Result<Value> {
        if !self
            .mailbox
            .lock()
            .map_err(|_error| Error::Storage)?
            .approvals
            .contains_key(&request_key(&id)?)
        {
            return Err(Error::Invalid);
        }
        self.send(&json!({"id":id,"result":{"decision":decision}}))
            .await?;
        self.mailbox
            .lock()
            .map_err(|_error| Error::Storage)?
            .remove_approval(&id)?;
        Ok(json!({"submitted":true}))
    }

    pub(super) async fn close(mut self) {
        self.cancel.cancel();
        if let Some(task) = self.task.take() {
            let _joined = task.await;
        }
    }
}

impl Drop for RemoteStream {
    fn drop(&mut self) {
        self.cancel.cancel();
        if let Some(task) = &self.task {
            task.abort();
        }
    }
}

#[cfg(test)]
#[path = "stream_tests.rs"]
mod tests;
