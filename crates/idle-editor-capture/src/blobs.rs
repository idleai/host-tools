//! Capture storage can stage bytes before its caller's durable commit.

use editchain_core::BlobRef;
use editchain_store::{BlobSource, BlobStorage};
use std::io;

/// Content storage used during editor conversion, including private previews.
pub trait CaptureBlobs: BlobSource {
    /// Retain exact bytes and return their full address and length.
    /// The caller must establish durability before acknowledging observations.
    /// # Errors
    /// Returns storage or reference-size errors.
    fn retain(&mut self, bytes: &[u8]) -> io::Result<BlobRef>;
}

impl<T: BlobStorage> CaptureBlobs for T {
    fn retain(&mut self, bytes: &[u8]) -> io::Result<BlobRef> {
        self.put(bytes)
    }
}
