//! Bounded, four-byte little-endian frames shared by native host services.

#[cfg(test)]
use tokio as _;

use std::io::{self, Read, Write};

#[cfg(feature = "async")]
pub mod asynchronous;

/// Read one frame, returning `None` only at a clean frame boundary.
/// # Errors
/// Returns truncated input, zero or excessive lengths, or transport errors.
pub fn read_frame(input: &mut impl Read, maximum: usize) -> io::Result<Option<Vec<u8>>> {
    let mut header = [0; 4];
    let [first, rest @ ..] = &mut header;
    match input.read_exact(std::slice::from_mut(first)) {
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        result => result?,
    }
    input.read_exact(rest)?;
    let mut bytes = vec![0; length(header, maximum)?];
    input.read_exact(&mut bytes)?;
    Ok(Some(bytes))
}

/// Write and flush one bounded frame without changing its payload.
/// # Errors
/// Returns invalid lengths or transport errors.
pub fn write_frame(output: &mut impl Write, bytes: &[u8], maximum: usize) -> io::Result<()> {
    output.write_all(&header(bytes.len(), maximum)?)?;
    output.write_all(bytes)?;
    output.flush()
}

fn length(header: [u8; 4], maximum: usize) -> io::Result<usize> {
    let length = usize::try_from(u32::from_le_bytes(header)).map_err(io::Error::other)?;
    validate(length, maximum)?;
    Ok(length)
}

fn header(length: usize, maximum: usize) -> io::Result<[u8; 4]> {
    validate(length, maximum)?;
    Ok(u32::try_from(length)
        .map_err(io::Error::other)?
        .to_le_bytes())
}

fn validate(length: usize, maximum: usize) -> io::Result<()> {
    if length == 0 || length > maximum {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "invalid host frame length",
        ));
    }
    Ok(())
}
