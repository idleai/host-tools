//! Compatibility for Microsoft's SSH fork using the fixed upstream secret buffer.

use std::ops::{Deref, DerefMut};

/// Original SSH buffer API backed by upstream's checked allocation/growth and
/// zeroization. No allocation or raw-memory operations are implemented here.
#[derive(Clone, Debug, Default)]
pub struct CryptoVec(upstream_cryptovec::CryptoVec);

impl CryptoVec {
    /// Create an empty secret buffer.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Create a zero-filled buffer using upstream's checked allocation.
    #[must_use]
    pub fn new_zeroed(size: usize) -> Self {
        Self(upstream_cryptovec::CryptoVec::new_zeroed(size))
    }

    /// Reserve capacity using upstream's checked allocation.
    #[must_use]
    pub fn with_capacity(capacity: usize) -> Self {
        Self(upstream_cryptovec::CryptoVec::with_capacity(capacity))
    }

    /// Copy bytes into zeroizing storage.
    #[must_use]
    pub fn from_slice(bytes: &[u8]) -> Self {
        Self(upstream_cryptovec::CryptoVec::from_slice(bytes))
    }

    /// Append the legacy SSH big-endian integer encoding.
    pub fn push_u32_be(&mut self, value: u32) {
        self.0.extend(&value.to_be_bytes());
    }

    /// Resize through upstream's checked growth, retaining two-phase borrowing
    /// for legacy expressions such as `buffer.resize(buffer.len())`.
    pub fn resize(&mut self, size: usize) {
        self.0.resize(size);
    }
}

impl Deref for CryptoVec {
    type Target = upstream_cryptovec::CryptoVec;
    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for CryptoVec {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

impl AsRef<[u8]> for CryptoVec {
    fn as_ref(&self) -> &[u8] {
        self.0.as_ref()
    }
}

impl AsMut<[u8]> for CryptoVec {
    fn as_mut(&mut self) -> &mut [u8] {
        self.0.as_mut()
    }
}

impl From<Vec<u8>> for CryptoVec {
    fn from(bytes: Vec<u8>) -> Self {
        Self(bytes.into())
    }
}

impl From<String> for CryptoVec {
    fn from(text: String) -> Self {
        Self(text.into())
    }
}

impl std::io::Write for CryptoVec {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.write(bytes)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.0.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::CryptoVec;

    #[test]
    fn growth_encoding_and_truncation_use_fixed_storage() {
        let mut bytes = CryptoVec::new();
        bytes.push_u32_be(0x1234_5678);
        assert_eq!(
            bytes.as_ref(),
            [0x12, 0x34, 0x56, 0x78],
            "legacy SSH encoding must stay unchanged"
        );
        bytes.resize(1);
        bytes.resize(4);
        assert_eq!(
            bytes.as_ref(),
            [0x12, 0, 0, 0],
            "discarded secret bytes must be erased"
        );
        bytes.extend(&vec![1; 65_536]);
        assert_eq!(
            bytes.len(),
            65_540,
            "upstream growth must preserve the original prefix"
        );
        assert!(
            CryptoVec::new_zeroed(0).is_empty(),
            "zero-length construction must be valid"
        );
        assert!(
            CryptoVec::with_capacity(0).is_empty(),
            "zero capacity must be valid"
        );
    }

    #[test]
    fn overflow_is_rejected_before_allocation_or_buffer_mutation() {
        let mut bytes = CryptoVec::from_slice(b"secret");
        let result =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| bytes.resize(usize::MAX)));
        assert!(
            result.is_err(),
            "impossible growth must be rejected before raw allocation"
        );
        assert_eq!(
            bytes.as_ref(),
            b"secret",
            "rejected growth must leave the existing buffer intact"
        );
    }
}
