use std::io;

use camino::Utf8PathBuf;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum ApiError {
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

    #[error("invalid {context}: {reason}")]
    InvalidData { context: String, reason: String },

    #[error("invalid JSON in {context}: {source}")]
    Json {
        context: String,
        #[source]
        source: serde_json::Error,
    },

    #[error("JWS signature verification failed: {reason}")]
    Signature { reason: String },

    #[error("Cannot download non-corrupt {url}!")]
    CannotDownload { url: String },
}

impl ApiError {
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

    pub(crate) fn invalid(context: impl Into<String>, reason: impl Into<String>) -> Self {
        Self::InvalidData {
            context: context.into(),
            reason: reason.into(),
        }
    }
}
