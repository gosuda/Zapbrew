//! Homebrew-style verbs: install, uninstall, upgrade, outdated, autoremove, cleanup, pin, deps, and related commands.

pub mod autoremove;
mod context;
pub mod dependency;
pub mod deps;
pub mod desc;
mod error;
pub mod fetch;
pub mod info;
pub mod install;
mod install_steps;
pub mod leaves;
pub mod link;
pub mod list;
pub mod outdated;
pub mod pin;
pub mod platform;
pub mod reinstall;
mod render;
pub mod search;
pub mod state;
mod transaction;
pub mod uninstall;
pub mod unlink;
pub mod unpin;
pub mod upgrade;
pub mod uses;

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

    pub fn fail_install_after_unlink(formula: &str) -> Result<(), OpError> {
        transaction::arm_install_failure_after_unlink(formula.to_owned())
    }

    pub fn fail_removal_after(formula: &str, staged: usize) -> Result<(), OpError> {
        transaction::arm_removal_failure_after(formula.to_owned(), staged)
    }
}
