//! Immutable workspace service installations supplied over the private host connection.

use serde::Deserialize;
use std::{io, path::PathBuf};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Open {
    pub(crate) workspace: PathBuf,
    pub(crate) service: Binding,
}

#[derive(Debug, Deserialize)]
#[serde(
    tag = "kind",
    content = "binding",
    rename_all = "snake_case",
    deny_unknown_fields
)]
pub(crate) enum Binding {
    Capture(idle_editor_capture::service::Binding),
    History(idle_history_native::history::service::Binding),
    Collection(idle_history_collector::Binding),
    Repository(idle_repository::Binding),
    Coordination(Box<idle_coordination::service::native::Configuration>),
    Runtime(idle_coordination::runtime::Binding),
}

impl Open {
    pub(crate) fn validate(&self) -> io::Result<()> {
        if !self.workspace.is_absolute() {
            return Err(io::Error::other("native workspace must be absolute"));
        }
        let valid = match &self.service {
            Binding::Capture(binding) => binding.workspace_path == self.workspace,
            Binding::Collection(binding) => binding.workspace == self.workspace,
            Binding::Repository(binding) => binding.root == self.workspace,
            Binding::History(binding) => binding.chain_directory.is_absolute(),
            Binding::Coordination(binding) => {
                binding.state_directory.is_absolute()
                    && binding.chain_directory.is_absolute()
                    && binding.device_directory.is_absolute()
            }
            Binding::Runtime(_) => true,
        };
        if !valid {
            return Err(io::Error::other(
                "native binding belongs to a different workspace",
            ));
        }
        Ok(())
    }
}

impl Binding {
    pub(crate) fn maximum(&self) -> usize {
        match self {
            Self::Capture(_) => 160 * 1024 * 1024,
            Self::History(_) => 8 * 1024 * 1024,
            Self::Collection(_) | Self::Runtime(_) => 1024 * 1024,
            Self::Repository(_) => 16 * 1024,
            Self::Coordination(_) => 16 * 1024 * 1024,
        }
    }
}
