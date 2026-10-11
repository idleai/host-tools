//! Standalone authority, transport lifecycle and native interoperability checks.

macro_rules! ensure {
    ($condition:expr, $($message:tt)+) => {
        if $condition { Ok::<(), std::io::Error>(()) }
        else { Err(std::io::Error::other(format!($($message)+))) }
    };
}

macro_rules! equal {
    ($left:expr, $right:expr, $message:expr $(,)?) => {
        match (&$left, &$right) {
            (left, right) if left == right => Ok::<(), std::io::Error>(()),
            (left, right) => Err(std::io::Error::other(format!(
                "{}: left={left:?}, right={right:?}",
                $message
            ))),
        }
    };
}

#[cfg(test)]
mod support;

#[cfg(test)]
#[path = "cases/authority.rs"]
mod authority;
#[cfg(test)]
#[path = "cases/native.rs"]
mod native;
#[cfg(test)]
#[path = "cases/peers.rs"]
mod peers;
#[cfg(test)]
#[path = "cases/runtime_owner.rs"]
mod runtime_owner;
#[cfg(test)]
#[path = "cases/runtime_transfer.rs"]
mod runtime_transfer;

#[cfg(test)]
#[path = "cases/workspace_config.rs"]
mod workspace_config;

use {
    editchain_sync as _, reqwest as _, russh as _, serde as _, sha2 as _, thiserror as _,
    tunnels as _, url as _,
};
