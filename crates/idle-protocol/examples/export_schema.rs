//! Export the checked-in v1 JSON Schema to a caller-supplied path.

use std::{error::Error, fs, io};

use idle_protocol::v1::api::WireMessage;
use serde as _;

fn main() -> Result<(), Box<dyn Error>> {
    let path = std::env::args_os().nth(1).ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "expected a schema output path")
    })?;
    let schema = schemars::schema_for!(WireMessage);
    fs::write(path, serde_json::to_string_pretty(&schema)?)?;
    Ok(())
}

#[cfg(feature = "reflection")]
use facet as _;
