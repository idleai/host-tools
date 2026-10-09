//! Separate Dev Tunnels transport for Codex workspace connections.
//!
//! The daemon owns grants and RPC authorization. This adapter carries bounded
//! messages and owns relay cleanup; it never runs models or accepts shell commands.

mod client;
mod credentials;
mod framing;
mod host;
mod invitation;

pub use client::{Binding, serve_client};
pub use host::serve_relay;
pub use invitation::RuntimeInvitation;

#[cfg(test)]
mod tests;
