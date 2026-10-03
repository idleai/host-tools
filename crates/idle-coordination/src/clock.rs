//! Authority clock injection; callers cannot set time in service requests.

use std::time::{SystemTime, UNIX_EPOCH};

use crate::{Error, Result};

/// Clock owned by the service host, separate from untrusted producer timestamps.
pub trait Clock: std::fmt::Debug + Send + Sync {
    /// Return Unix milliseconds.
    ///
    /// # Errors
    /// Fails closed if a usable authority time is unavailable.
    fn now_ms(&self) -> Result<u64>;
}

/// Operating system clock for native hosts.
#[derive(Clone, Copy, Debug, Default)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now_ms(&self) -> Result<u64> {
        u64::try_from(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|_error| Error::Invalid)?
                .as_millis(),
        )
        .map_err(|_error| Error::Invalid)
    }
}
