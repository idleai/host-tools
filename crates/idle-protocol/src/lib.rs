//! Versioned, transport-independent coordination contracts for Idle.
//!
//! Clients, Evo and standalone or managed providers share [`v1`]. The crate
//! contains wire data and scope/correlation helpers, not services, authentication
//! implementations, runtime enforcement or UI state. Enable `schema` to export
//! JSON Schema; ordinary consumers need only Serde.
//!
//! This independent package lives in the host-tools repository. Consumers depend
//! directly on `idle-protocol`; the package has no dependency on the application
//! crate, Crux, an engine implementation or private backend source.
//!
//! See the packaged `docs/v1.md` for normative retry, authority and recovery rules.

#[cfg(feature = "fixtures")]
pub mod fixtures;
pub mod v1;

#[cfg(test)]
mod tests;
