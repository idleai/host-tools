//! Repository-scoped coordination and native, authenticated history replication.

pub mod authority;
pub mod clock;
pub mod dev_tunnels;
pub mod discovery;
pub mod engine;
mod error;
pub mod invitation;
pub mod peer;
pub mod persistence;
pub mod runtime;
pub mod service;
pub mod transport;
pub mod workspace_config;

pub use error::{Error, Result};

#[cfg(test)]
use {editchain_core as _, editchain_store as _};
