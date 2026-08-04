use std::io;

use camino::Utf8PathBuf;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum PrefixError {
    #[error("failed to {operation} {path}: {source}")]
    Io {
        operation: &'static str,
        path: Utf8PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("failed to parse receipt {path}: {source}")]
    ReceiptJson {
        path: Utf8PathBuf,
        #[source]
        source: serde_json::Error,
    },

    #[error("environment variable {name} is invalid: {value}")]
    InvalidEnvironment { name: &'static str, value: String },

    #[error("unsupported host {os}/{arch}")]
    UnsupportedHost { os: String, arch: String },

    #[error("failed to run `{program}`: {source}")]
    CommandIo {
        program: String,
        #[source]
        source: io::Error,
    },

    #[error("command `{program}` failed with status {status}: {stderr}")]
    CommandFailed {
        program: String,
        status: String,
        stderr: String,
    },

    #[error("{kind} is not a safe path segment: {value}")]
    InvalidPathSegment { kind: &'static str, value: String },

    #[error(
        "A `{command}` process has already locked {path}. Please wait for it to finish or terminate it to continue."
    )]
    LockBusy { command: String, path: Utf8PathBuf },

    #[error("Unable to locate the system's dynamic linker")]
    NoSystemLdSo,
}

impl PrefixError {
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
