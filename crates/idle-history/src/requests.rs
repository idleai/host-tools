//! Correlated request ownership shared by Crux and transitional host adapters.

use std::collections::BTreeMap;

/// Largest integer that every JavaScript host can transport without rounding.
const MAX_REQUEST_ID: u64 = 9_007_199_254_740_991;

/// A request could not acquire ownership; no pending work was changed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RequestError {
    /// The single window/reconciliation slot already has an owner.
    WindowBusy,
    /// Request identities cannot be reused during this client's lifetime.
    Exhausted,
}

/// Monotonic request identities and optional exclusive window ownership.
/// Clearing a context retires its requests without recycling any identities.
#[derive(Clone, Debug)]
pub struct RequestTracker<T> {
    next: Option<u64>,
    pending: BTreeMap<u64, T>,
    window: Option<u64>,
    retained: Option<usize>,
}

impl<T> Default for RequestTracker<T> {
    fn default() -> Self {
        Self {
            next: Some(1),
            pending: BTreeMap::new(),
            window: None,
            retained: None,
        }
    }
}

impl<T> RequestTracker<T> {
    /// Retain only this many diagnostic envelopes, protecting the window owner.
    /// Use the default unbounded tracker for requests that must all complete.
    #[must_use]
    pub fn bounded(retained: usize) -> Self {
        Self {
            retained: Some(retained.max(2)),
            ..Self::default()
        }
    }

    /// Establish ownership before dispatching a possibly synchronous host call.
    ///
    /// # Errors
    /// Returns an error if the exclusive slot is busy or identities are exhausted.
    pub fn register(&mut self, value: T, window: bool) -> Result<u64, RequestError> {
        if window && self.window.is_some() {
            return Err(RequestError::WindowBusy);
        }
        let id = self.next.ok_or(RequestError::Exhausted)?;
        self.next = id.checked_add(1).filter(|next| *next <= MAX_REQUEST_ID);
        if self
            .retained
            .is_some_and(|limit| self.pending.len() >= limit)
            && let Some(oldest) = self
                .pending
                .keys()
                .copied()
                .find(|id| Some(*id) != self.window)
        {
            drop(self.pending.remove(&oldest));
        }
        drop(self.pending.insert(id, value));
        if window {
            self.window = Some(id);
        }
        Ok(id)
    }

    /// Consume a result exactly once, releasing only its own exclusive slot.
    pub fn take(&mut self, id: u64) -> Option<(T, bool)> {
        let value = self.pending.remove(&id)?;
        let window = self.window == Some(id);
        if window {
            self.window = None;
        }
        Some((value, window))
    }

    /// Invalidate all work from the previous context, including its window slot.
    pub fn clear(&mut self) {
        self.pending.clear();
        self.window = None;
    }

    /// Retire superseded requests while preserving unrelated operations.
    pub fn retain(&mut self, mut keep: impl FnMut(&T) -> bool) {
        self.pending.retain(|_, value| keep(value));
        if self
            .window
            .is_some_and(|id| !self.pending.contains_key(&id))
        {
            self.window = None;
        }
    }

    /// Pending request values, for deduplication and invalidation.
    pub fn values(&self) -> impl Iterator<Item = &T> {
        self.pending.values()
    }

    /// Look up a still-owned request without consuming it.
    #[must_use]
    pub fn get(&self, id: u64) -> Option<&T> {
        self.pending.get(&id)
    }

    /// Whether the request still belongs to this context.
    #[must_use]
    pub fn contains(&self, id: u64) -> bool {
        self.pending.contains_key(&id)
    }

    /// Current exclusive window owner.
    #[must_use]
    pub const fn pending_window(&self) -> Option<u64> {
        self.window
    }

    /// Number of retained requests.
    #[must_use]
    pub fn len(&self) -> usize {
        self.pending.len()
    }

    /// Whether all requests have completed or been retired.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::{MAX_REQUEST_ID, RequestError, RequestTracker};

    #[test]
    fn retiring_and_replay_cannot_steal_current_window() {
        let mut tracker = RequestTracker::bounded(3);
        let old = tracker.register("old", true).expect("old window");
        tracker.clear();
        let current = tracker.register("current", true).expect("current window");
        for _ in 0..20 {
            let _id = tracker.register("search", false).expect("search request");
        }
        assert_eq!(tracker.len(), 3, "diagnostic requests are bounded");
        assert!(tracker.take(old).is_none(), "retired context is ignored");
        assert_eq!(
            tracker.pending_window(),
            Some(current),
            "current window survives"
        );
        assert_eq!(
            tracker.take(current),
            Some(("current", true)),
            "consume once"
        );
        assert!(
            tracker.take(current).is_none(),
            "duplicate response is ignored"
        );
    }

    #[test]
    fn exhaustion_and_busy_slot_do_not_replace_requests() {
        let mut tracker = RequestTracker {
            next: Some(MAX_REQUEST_ID),
            ..RequestTracker::default()
        };
        assert_eq!(
            tracker.register("last", true),
            Ok(MAX_REQUEST_ID),
            "last safe ID"
        );
        assert_eq!(
            tracker.register("busy", true),
            Err(RequestError::WindowBusy),
            "one window"
        );
        assert_eq!(
            tracker.register("overflow", false),
            Err(RequestError::Exhausted),
            "never wrap"
        );
        tracker.clear();
        assert_eq!(
            tracker.register("reset", false),
            Err(RequestError::Exhausted),
            "reset cannot reuse IDs"
        );
    }
}
