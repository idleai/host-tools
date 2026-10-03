//! Export the additive standalone repository coordination schema.

use std::{error::Error, fs, io};

use idle_protocol::v1::standalone::RepositoryMessage;
use serde as _;

fn main() -> Result<(), Box<dyn Error>> {
    let path = std::env::args_os().nth(1).ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "expected a schema output path")
    })?;
    fs::write(
        path,
        serde_json::to_string_pretty(&schemars::schema_for!(RepositoryMessage))?,
    )?;
    Ok(())
}
