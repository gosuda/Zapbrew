use std::fs;

use zapbrew_prefix::{Keg, pin};

use crate::link::{canonical_names, selected_keg};
use crate::state::scan_selected;
use crate::transaction::acquire_formula_locks;
use crate::{Ctx, OpError};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Args {
    pub names: Vec<String>,
}

pub async fn run(ctx: &Ctx, args: Args) -> Result<(), OpError> {
    let names = canonical_names(&args.names, "pin")?;
    let locks = acquire_formula_locks(&ctx.env, &names)?;
    let state = scan_selected(&ctx.env, &names)?;

    for name in &names {
        let installed = state.formula(name).ok_or_else(|| OpError::Refusal {
            message: format!("{name} not installed"),
        })?;
        let record = ctx.env.pins.join(name);
        if fs::symlink_metadata(record.as_std_path())
            .map(|metadata| metadata.file_type().is_symlink())
            .unwrap_or(false)
        {
            ctx.reporter.opoo(&format!("{name} already pinned"));
            continue;
        }

        let selected = selected_keg(installed).ok_or_else(|| OpError::Refusal {
            message: format!("{name} not installed"),
        })?;
        let keg = Keg::new(
            &ctx.env.cellar,
            installed.name().clone(),
            selected.version().clone(),
        )?;
        pin(&ctx.env.pins, &keg)?;
    }

    drop(locks);
    Ok(())
}
