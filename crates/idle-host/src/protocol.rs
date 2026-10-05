//! A routing header around unchanged service payloads; no JSON numbers are rewritten.

use std::io;

pub(crate) const VERSION: u8 = 1;
pub(crate) const HEADER: usize = 8;
pub(crate) const MAX_PAYLOAD: usize = 160 * 1024 * 1024;
pub(crate) const MAX_FRAME: usize = MAX_PAYLOAD + HEADER;
pub(crate) const MAX_OPEN: usize = 64 * 1024;
pub(crate) const MAX_CHANNELS: usize = 64;
pub(crate) const MAX_QUEUED_BYTES: usize = 192 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[repr(u8)]
pub(crate) enum Kind {
    Hello = 0,
    Open = 1,
    Data = 2,
    Close = 3,
    Ready = 4,
    Closed = 5,
    Shutdown = 6,
}

impl TryFrom<u8> for Kind {
    type Error = io::Error;

    fn try_from(value: u8) -> Result<Self, Self::Error> {
        match value {
            0 => Ok(Self::Hello),
            1 => Ok(Self::Open),
            2 => Ok(Self::Data),
            3 => Ok(Self::Close),
            4 => Ok(Self::Ready),
            5 => Ok(Self::Closed),
            6 => Ok(Self::Shutdown),
            _ => Err(invalid()),
        }
    }
}

impl Kind {
    fn code(self) -> u8 {
        match self {
            Self::Hello => 0,
            Self::Open => 1,
            Self::Data => 2,
            Self::Close => 3,
            Self::Ready => 4,
            Self::Closed => 5,
            Self::Shutdown => 6,
        }
    }
}

#[derive(Debug)]
pub(crate) struct Frame {
    pub(crate) kind: Kind,
    pub(crate) channel: u32,
    pub(crate) payload: Vec<u8>,
}

impl Frame {
    pub(crate) fn decode(bytes: &[u8]) -> io::Result<Self> {
        let (header, payload) = bytes.split_first_chunk::<HEADER>().ok_or_else(invalid)?;
        let [version, kind, reserved_a, reserved_b, a, b, c, d] = *header;
        if version != VERSION || reserved_a != 0 || reserved_b != 0 || payload.len() > MAX_PAYLOAD {
            return Err(invalid());
        }
        Ok(Self {
            kind: Kind::try_from(kind)?,
            channel: u32::from_le_bytes([a, b, c, d]),
            payload: payload.to_vec(),
        })
    }

    pub(crate) fn encode(&self) -> Vec<u8> {
        let [a, b, c, d] = self.channel.to_le_bytes();
        let mut bytes = Vec::with_capacity(HEADER.saturating_add(self.payload.len()));
        bytes.extend_from_slice(&[VERSION, self.kind.code(), 0, 0, a, b, c, d]);
        bytes.extend_from_slice(&self.payload);
        bytes
    }

    pub(crate) fn control(kind: Kind, channel: u32) -> Self {
        Self {
            kind,
            channel,
            payload: Vec::new(),
        }
    }

    pub(crate) fn closed(channel: u32, code: &str) -> Self {
        Self {
            kind: Kind::Closed,
            channel,
            payload: format!("{{\"code\":\"{code}\"}}").into_bytes(),
        }
    }
}

pub(crate) fn invalid() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "invalid native host frame")
}
