//! Homebrew-style verbs: install, uninstall, upgrade, outdated, autoremove, cleanup, pin, deps, and related commands.

mod context;
pub mod dependency;
mod error;
pub mod fetch;
pub mod install;
mod install_steps;
pub mod platform;
pub mod reinstall;
pub mod state;
mod transaction;

pub use context::{Ctx, Reporter};
pub use error::OpError;

#[doc(hidden)]
pub mod transaction_test_support {
    use camino::{Utf8Path, Utf8PathBuf};

    use crate::{OpError, transaction};

    pub fn force_next_stage_id(rack: &Utf8Path, id: u64) -> Result<Utf8PathBuf, OpError> {
        transaction::arm_stage_collision(rack.to_path_buf(), id)
    }

    pub fn fail_next_backup_cleanup(formula: &str) -> Result<(), OpError> {
        transaction::arm_cleanup_failure(formula.to_owned())
    }
}
