//! Shared byte budgets bound queued messages across all workspace channels.

use super::protocol::{Frame, MAX_QUEUED_BYTES};
use std::{io, sync::Arc};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc};

pub(crate) struct Queued<T> {
    pub(crate) value: T,
    _permit: OwnedSemaphorePermit,
}

impl<T> Queued<T> {
    pub(crate) fn new(value: T, size: usize, budget: &Arc<Semaphore>) -> io::Result<Self> {
        let permit = budget
            .clone()
            .try_acquire_many_owned(u32::try_from(size.max(1)).map_err(io::Error::other)?)
            .map_err(|_error| io::Error::other("native host queue is full"))?;
        Ok(Self {
            value,
            _permit: permit,
        })
    }
}

#[derive(Clone)]
pub(crate) struct Output {
    sender: mpsc::Sender<Queued<Frame>>,
    budget: Arc<Semaphore>,
}

impl Output {
    pub(crate) fn new() -> (Self, mpsc::Receiver<Queued<Frame>>) {
        let (sender, receiver) = mpsc::channel(64);
        (
            Self {
                sender,
                budget: Arc::new(Semaphore::new(MAX_QUEUED_BYTES)),
            },
            receiver,
        )
    }

    pub(crate) async fn send(&self, frame: Frame) -> io::Result<()> {
        let size = frame.payload.len();
        let queued = Queued::new(frame, size, &self.budget)?;
        self.sender.send(queued).await.map_err(io::Error::other)
    }

    pub(crate) fn blocking_send(&self, frame: Frame) -> io::Result<()> {
        let size = frame.payload.len();
        let queued = Queued::new(frame, size, &self.budget)?;
        self.sender.blocking_send(queued).map_err(io::Error::other)
    }
}
