use zapbrew_pour::{plan_unlink, unlink};
use zapbrew_prefix::{Keg, Prefix};

use crate::link::{canonical_names, selected_keg};
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
    let locks = acquire_formula_locks(&ctx.env, &names)?;
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
