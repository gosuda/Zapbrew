use std::io;

use camino::Utf8PathBuf;
use thiserror::Error;
use zapbrew_types::{BottleTag, Checksum};

#[derive(Debug, Error)]
pub enum NetError {
    #[error("failed to {operation} {path}: {source}")]
    Io {
        operation: &'static str,
        path: Utf8PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("request failed for {url}: {source}")]
    Http {
        url: String,
        #[source]
        source: reqwest::Error,
    },

    #[error("invalid response from {url}: {reason}")]
    InvalidResponse { url: String, reason: String },

    #[error("{name}: no bottle available for {tag}. brew can build from source; zapbrew cannot.")]
    NoBottle { name: String, tag: BottleTag },

    #[error(
        "SHA-256 mismatch\nExpected: {expected}\n  Actual: {actual}\n    File: {path}\nTo retry an incomplete download, remove the file above."
    )]
    ChecksumMismatch {
        expected: Checksum,
        actual: Checksum,
        path: Utf8PathBuf,
    },

    #[error("{kind} is not a safe path segment: {value}")]
    InvalidPathSegment { kind: &'static str, value: String },
}

impl NetError {
    pub(crate) fn io(
        operation: &'static str,
        path: impl Into<Utf8PathBuf>,
        source: io::Error,
    ) -> Self {
        Self::Io {
            operation,
            path: path.into(),
            source,
        }
    }
}
