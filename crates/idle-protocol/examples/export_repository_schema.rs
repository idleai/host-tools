//! Export the independent standalone repository snapshot schema.

#[cfg(feature = "reflection")]
use facet as _;
use idle_protocol::v1::repository::RepositorySnapshot;
use serde as _;
use std::{error::Error, fs, io};

fn main() -> Result<(), Box<dyn Error>> {
    let path = std::env::args_os().nth(1).ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "expected a schema output path")
    })?;
    fs::write(
        path,
        serde_json::to_string_pretty(&schemars::schema_for!(RepositorySnapshot))?,
    )?;
    Ok(())
}
