use std::fs;

use camino::Utf8Path;
use zapbrew_pour::{plan_unlink, unlink};
use zapbrew_prefix::{Keg, Prefix, Tab};

use crate::link::{canonical_names, resolves_inside, selected_keg};
use crate::state::scan_selected;
use crate::transaction::acquire_formula_locks;
use crate::{Ctx, OpError};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Args {
    pub names: Vec<String>,
    pub dry_run: bool,
}

pub async fn run(ctx: &Ctx, args: Args) -> Result<(), OpError> {
    let names = canonical_names(&args.names, "unlink")?;
    // A preview must not create a lock file or contend with a real operation.
    let locks = if args.dry_run {
        None
    } else {
        Some(acquire_formula_locks(&ctx.env, &names)?)
    };
    let state = scan_selected(&ctx.env, &names)?;
    let prefix = Prefix::new(ctx.env.clone());

    for name in &names {
        let installed = state.formula(name).ok_or_else(|| OpError::Refusal {
            message: format!("No such keg: {}/{name}", ctx.env.cellar),
        })?;
        let selected = installed
            .linked()
            .or_else(|| selected_keg(installed))
            .ok_or_else(|| OpError::Refusal {
                message: format!("No such keg: {}/{name}", ctx.env.cellar),
            })?;
        let keg = Keg::new(
            &ctx.env.cellar,
            installed.name().clone(),
            selected.version().clone(),
        )?;

        if args.dry_run {
            let report = plan_unlink(&keg, &prefix)?;
            ctx.reporter.print("Would remove:");
            for path in &report.removed {
                ctx.reporter.print(path.as_str());
            }
            for path in &report.pruned {
                ctx.reporter.print(path.as_str());
            }
        } else {
            remove_alias_opt_symlinks(ctx, &prefix, &keg, name)?;
            let report = unlink(&keg, &prefix)?;
            ctx.reporter.print(&format!(
                "Unlinking {}... {} symlinks removed.",
                keg.path(),
                report.removed.len()
            ));
        }
    }

    drop(locks);
    Ok(())
}

/// Remove `opt/<alias>` and `linked/<alias>` symlinks that resolve into `keg`,
/// for every alias and oldname of the formula. Mirrors Homebrew's
/// `remove_old_aliases` / `remove_oldname_opt_records`. Called before the
/// linked-keg record is dropped so the alias symlinks still resolve.
fn remove_alias_opt_symlinks(
    ctx: &Ctx,
    prefix: &Prefix,
    keg: &Keg,
    name: &str,
) -> Result<(), OpError> {
    let opt_dir = prefix.opt();
    let linked_dir = prefix.linked();
    for alias in alias_names(ctx, keg, name) {
        if alias == name {
            continue;
        }
        remove_alias_if_resolves(&opt_dir.join(&alias), keg.path())?;
        remove_alias_if_resolves(&linked_dir.join(&alias), keg.path())?;
    }
    Ok(())
}

/// Collect alias and oldname strings for the formula behind `keg`. Prefers the
/// live catalog; falls back to the aliases recorded on the keg's tab (oldnames
/// are not stored on the tab).
fn alias_names(ctx: &Ctx, keg: &Keg, name: &str) -> Vec<String> {
    if let Some(formula) = ctx.catalog.get(name) {
        return formula
            .aliases
            .iter()
            .chain(formula.oldnames.iter())
            .cloned()
            .collect();
    }
    Tab::load(keg.receipt_path())
        .map(|tab| tab.aliases)
        .unwrap_or_default()
}

/// Remove `path` if it is a symlink whose resolved target falls inside `keg`.
fn remove_alias_if_resolves(path: &Utf8Path, keg: &Utf8Path) -> Result<(), OpError> {
    let Ok(meta) = fs::symlink_metadata(path.as_std_path()) else {
        return Ok(());
    };
    if !meta.file_type().is_symlink() {
        return Ok(());
    }
    if resolves_inside(path, keg) {
        fs::remove_file(path.as_std_path())
            .map_err(|source| OpError::io("remove", path, source))?;
    }
    Ok(())
}
