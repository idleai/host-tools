//! Sync and async adapters preserve frame boundaries and reject incomplete input.

use idle_host_io::{read_frame, write_frame};
use std::io::{self, Read};
use tokio as _;

macro_rules! verify {
    ($condition:expr, $message:expr $(,)?) => {
        if !$condition {
            return Err(io::Error::other($message));
        }
    };
}
macro_rules! verify_eq {
    ($actual:expr, $expected:expr, $message:expr $(,)?) => {
        if $actual != $expected {
            return Err(io::Error::other($message));
        }
    };
}

fn invalid_inputs() -> Vec<(Vec<u8>, io::ErrorKind)> {
    vec![
        (vec![1], io::ErrorKind::UnexpectedEof),
        (vec![1, 0, 0], io::ErrorKind::UnexpectedEof),
        (vec![1, 0, 0, 0], io::ErrorKind::UnexpectedEof),
        (vec![0, 0, 0, 0], io::ErrorKind::InvalidData),
        (vec![5, 0, 0, 0], io::ErrorKind::InvalidData),
    ]
}

#[test]
fn blocking_frames_handle_partial_reads_limits_and_clean_eof() -> io::Result<()> {
    struct ByteReader<'a>(&'a [u8]);
    impl Read for ByteReader<'_> {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            match buffer.first_mut() {
                Some(first) => self.0.read(std::slice::from_mut(first)),
                None => Ok(0),
            }
        }
    }
    let mut output = Vec::new();
    write_frame(&mut output, b"four", 4)?;
    write_frame(&mut output, b"x", 4)?;
    let mut input = ByteReader(&output);
    verify_eq!(
        read_frame(&mut input, 4)?,
        Some(b"four".to_vec()),
        "inclusive size limit"
    );
    verify_eq!(
        read_frame(&mut input, 4)?,
        Some(b"x".to_vec()),
        "next frame stays separate"
    );
    verify_eq!(
        read_frame(&mut input, 4)?,
        None,
        "EOF after complete frames"
    );
    for (bytes, kind) in invalid_inputs() {
        let result = read_frame(&mut bytes.as_slice(), 4);
        verify_eq!(
            result.err().map(|error| error.kind()),
            Some(kind),
            "reject invalid input"
        );
    }
    for bytes in [b"".as_slice(), b"large".as_slice()] {
        let mut output = Vec::new();
        verify!(
            write_frame(&mut output, bytes, 4).is_err(),
            "invalid output rejected"
        );
        verify!(output.is_empty(), "invalid output writes no partial header");
    }
    Ok(())
}

#[cfg(feature = "async")]
#[tokio::test]
async fn async_frames_match_the_blocking_codec() -> io::Result<()> {
    use idle_host_io::asynchronous;
    let mut output = Vec::new();
    asynchronous::write_frame(&mut output, b"four", 4).await?;
    let mut expected = Vec::new();
    write_frame(&mut expected, b"four", 4)?;
    verify_eq!(output, expected, "wire bytes match exactly");
    let mut input = output.as_slice();
    verify_eq!(
        asynchronous::read_frame(&mut input, 4).await?,
        Some(b"four".to_vec()),
        "inclusive size limit"
    );
    verify_eq!(
        asynchronous::read_frame(&mut input, 4).await?,
        None,
        "clean EOF"
    );
    for (bytes, kind) in invalid_inputs() {
        let result = asynchronous::read_frame(&mut bytes.as_slice(), 4).await;
        verify_eq!(
            result.err().map(|error| error.kind()),
            Some(kind),
            "same invalid input rules"
        );
    }
    for bytes in [b"".as_slice(), b"large".as_slice()] {
        let mut output = Vec::new();
        verify!(
            asynchronous::write_frame(&mut output, bytes, 4)
                .await
                .is_err(),
            "invalid output rejected"
        );
        verify!(output.is_empty(), "invalid output writes no partial header");
    }
    Ok(())
}
