//! Shared native history operations on the host that owns the records.
//! Clients own platform actions and translate these results into application state.

#[cfg(feature = "service")]
pub mod activity;
#[cfg(feature = "service")]
pub mod history;
pub mod projections;
pub mod query;
