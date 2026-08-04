//! Homebrew-style verbs: install, uninstall, upgrade, outdated, autoremove, cleanup, pin, deps, and related commands.

pub mod autoremove;
pub mod cask;
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
pub mod services;
pub mod shellenv;
pub mod shim;
mod size;
pub mod state;
pub mod tap;
pub mod tap_info;
mod transaction;
pub mod uninstall;
pub mod unlink;
pub mod unpin;
pub mod untap;
pub mod update;
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
        let has_findings = !findings.is_empty();
        crate::doctor::report(ctx, findings);
        if has_findings {
            Err(OpError::DoctorProblemsFound)
        } else {
            Ok(())
        }
    }
}

#[doc(hidden)]
pub mod shellenv_test_support {
    use std::ffi::OsStr;

    use crate::{Ctx, OpError, shellenv};

    pub async fn run_with_detected(
        ctx: &Ctx,
        args: shellenv::Args,
        detected: Option<&str>,
    ) -> Result<(), OpError> {
        shellenv::run_with_detected(ctx, args, detected.map(OsStr::new)).await
    }
}

#[doc(hidden)]
pub mod services_test_support {
    use serde_json::Value;
    use zapbrew_prefix::Env;

    use crate::{OpError, services};

    pub fn parse(env: &Env, name: &str, value: &Value) -> Result<(), OpError> {
        services::parse_for_test(env, name, value)
    }

    pub fn render_systemd_unit(env: &Env, name: &str, value: &Value) -> Result<String, OpError> {
        services::render_systemd_unit_for_test(env, name, value)
    }

    pub fn render_systemd_timer(env: &Env, name: &str, value: &Value) -> Result<String, OpError> {
        services::render_systemd_timer_for_test(env, name, value)
    }

    pub fn render_launchd_plist(env: &Env, name: &str, value: &Value) -> Result<String, OpError> {
        services::render_launchd_plist_for_test(env, name, value)
    }
}

#[doc(hidden)]
pub mod update_test_support {
    use zapbrew_api::RefreshReport;

    use crate::{Ctx, OpError, update};

    pub fn run_with_report(ctx: &Ctx, report: &RefreshReport) -> Result<(), OpError> {
        update::run_with_report(ctx, report)
    }
}

#[doc(hidden)]
pub mod shim_test_support {
    use camino::Utf8Path;

    use crate::{Ctx, OpError, shim};

    pub fn run_with_exe(
        ctx: &Ctx,
        action: shim::ShimAction,
        exe: &Utf8Path,
    ) -> Result<(), OpError> {
        shim::run_with_exe(ctx, shim::Args { action }, exe)
    }
}
