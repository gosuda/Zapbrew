//! Homebrew-style verbs: install, uninstall, upgrade, outdated, autoremove, cleanup, pin, deps, and related commands.

pub mod autoremove;
pub mod cleanup;
pub mod config;
mod context;
pub mod dependency;
pub mod deps;
pub mod desc;
pub mod doctor;
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
mod size;
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

#[doc(hidden)]
pub mod cleanup_test_support {
    pub fn older_than(mtime: i64, ctime: i64, now: i64, days: u64) -> bool {
        crate::cleanup::older_than(mtime, ctime, now, days)
    }
}

#[doc(hidden)]
pub mod config_test_support {
    use std::collections::BTreeMap;

    use crate::Ctx;

    pub fn lines(ctx: &Ctx, vars: &BTreeMap<String, String>, cores: usize) -> Vec<String> {
        crate::config::lines(ctx, vars, cores)
    }
}

#[doc(hidden)]
pub mod doctor_test_support {
    use std::collections::BTreeSet;

    use camino::{Utf8Path, Utf8PathBuf};

    use crate::{Ctx, OpError};

    pub fn findings(
        ctx: &Ctx,
        path_entries: &[Utf8PathBuf],
        unwritable: &BTreeSet<Utf8PathBuf>,
    ) -> Result<Vec<String>, OpError> {
        crate::doctor::findings(ctx, path_entries, &|path: &Utf8Path| {
            !unwritable.contains(path)
        })
    }

    pub fn run_with(
        ctx: &Ctx,
        path_entries: &[Utf8PathBuf],
        unwritable: &BTreeSet<Utf8PathBuf>,
    ) -> Result<(), OpError> {
        let findings = findings(ctx, path_entries, unwritable)?;
        crate::doctor::report(ctx, findings);
        Ok(())
    }
}
