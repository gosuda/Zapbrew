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

    #[error("invalid post_install_steps for {formula} at index {index} ({step_type}): {reason}")]
    InstallStep {
        formula: String,
        index: usize,
        step_type: String,
        reason: String,
    },

    #[error(
        "installed keg {keg} is active but cleanup is incomplete; surviving paths: {}",
        .leftovers.iter().map(|path| path.as_str()).collect::<Vec<_>>().join(", ")
    )]
    CleanupIncomplete {
        keg: Utf8PathBuf,
        leftovers: Vec<Utf8PathBuf>,
    },

    #[error("doctor found problems")]
    DoctorProblemsFound,
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
