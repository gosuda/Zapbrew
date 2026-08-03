use std::io;

use camino::Utf8PathBuf;
use thiserror::Error;

/// Errors surfaced by operation entry points.
#[derive(Debug, Error)]
pub enum OpError {
    #[error(transparent)]
    Api(#[from] zapbrew_api::ApiError),

    #[error(transparent)]
    Net(#[from] zapbrew_net::NetError),

    #[error(transparent)]
    Pour(#[from] zapbrew_pour::PourError),

    #[error(transparent)]
    Prefix(#[from] zapbrew_prefix::PrefixError),

    #[error("failed to {operation} {path}: {source}")]
    Io {
        operation: &'static str,
        path: Utf8PathBuf,
        #[source]
        source: io::Error,
    },

    #[error("command `{program}` failed with status {status}: {stderr}")]
    CommandFailed {
        program: String,
        status: String,
        stderr: String,
    },

    #[error("No available formula with the name \"{name}\".")]
    MissingFormula { name: String },

    #[error("invalid operation state: {reason}")]
    InvalidState { reason: String },

    #[error("dependency cycle detected: {}", cycle.join(" -> "))]
    DependencyCycle { cycle: Vec<String> },

    #[error("{message}")]
    Refusal { message: String },

    #[error("{original}; rollback incomplete; leftovers: {leftovers}")]
    RollbackIncomplete {
        original: Box<OpError>,
        leftovers: String,
    },
}

impl OpError {
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
