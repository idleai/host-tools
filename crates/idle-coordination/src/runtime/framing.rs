use std::io;

use serde::{Serialize, de::DeserializeOwned};
use tokio::io::{AsyncRead, AsyncReadExt as _, AsyncWrite, AsyncWriteExt as _};

pub(super) const MAX_FRAME: usize = 1024 * 1024;

pub(super) async fn read<T: DeserializeOwned>(
    input: &mut (impl AsyncRead + Unpin),
) -> io::Result<Option<T>> {
    let first = match input.read_u8().await {
        Ok(first) => first,
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(error) => return Err(error),
    };
    let mut rest = [0; 3];
    let _read = input.read_exact(&mut rest).await?;
    let [a, b, c] = rest;
    let length = usize::try_from(u32::from_be_bytes([first, a, b, c])).map_err(io::Error::other)?;
    if length == 0 || length > MAX_FRAME {
        return Err(invalid());
    }
    let mut bytes = vec![0; length];
    let _read = input.read_exact(&mut bytes).await?;
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|_error| invalid())
}

pub(super) async fn write<T: Serialize>(
    output: &mut (impl AsyncWrite + Unpin),
    value: &T,
) -> io::Result<()> {
    let bytes = serde_json::to_vec(value).map_err(|_error| invalid())?;
    if bytes.len() > MAX_FRAME {
        return Err(invalid());
    }
    let length = u32::try_from(bytes.len()).map_err(io::Error::other)?;
    output.write_all(&length.to_be_bytes()).await?;
    output.write_all(&bytes).await?;
    output.flush().await
}

pub(super) fn invalid() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "invalid Idle runtime frame")
}
