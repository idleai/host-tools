//! Shared CLI formatting with the application diagnostic prefix.

pub(crate) use editchain_cli_support::output::{Format, Output};
use std::io::{self, Write};

pub(crate) fn diagnostic(message: &str) -> io::Result<()> {
    writeln!(io::stderr().lock(), "idle-history-tools: {message}")
}
