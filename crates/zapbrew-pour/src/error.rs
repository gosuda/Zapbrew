use std::io;

use camino::Utf8PathBuf;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum PourError {
    #[error("failed to {operation} {path}: {source}")]
    Io {
        operation: &'static str,
        path: Utf8PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("invalid bottle archive entry {path}: {reason}")]
    InvalidArchive { path: String, reason: String },

    #[error("cannot relocate {path}: {reason}")]
    Relocation { path: Utf8PathBuf, reason: String },

    #[error("Could not symlink {link_source}\nTarget {target} {reason}")]
    LinkConflict {
        link_source: Utf8PathBuf,
        target: Utf8PathBuf,
        reason: String,
    },

    #[error("command `{program}` failed with status {status}: {stderr}")]
    CommandFailed {
        program: String,
        status: String,
        stderr: String,
    },

    #[error(transparent)]
    Prefix(#[from] zapbrew_prefix::PrefixError),
}

impl PourError {
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
