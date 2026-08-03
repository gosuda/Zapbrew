use std::fs;
use std::str::FromStr;

use zapbrew_prefix::unpin;
use zapbrew_types::FormulaName;

use crate::link::canonical_names;
use crate::state::scan_selected;
use crate::transaction::acquire_formula_locks;
use crate::{Ctx, OpError};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Args {
    pub names: Vec<String>,
}

pub async fn run(ctx: &Ctx, args: Args) -> Result<(), OpError> {
    let names = canonical_names(&args.names, "unpin")?;
    let locks = acquire_formula_locks(&ctx.env, &names)?;
    let state = scan_selected(&ctx.env, &names)?;

    for name in &names {
        let formula_name = FormulaName::from_str(name).map_err(|source| OpError::InvalidState {
            reason: format!("formula name {name} is invalid: {source}"),
        })?;
        let record = ctx.env.pins.join(name);
        let pinned = fs::symlink_metadata(record.as_std_path())
            .map(|metadata| metadata.file_type().is_symlink())
            .unwrap_or(false);

        if pinned {
            unpin(&ctx.env.pins, &formula_name)?;
        } else if state.formula(name).is_none() {
            ctx.reporter.onoe(&format!("{name} not installed"));
        } else {
            ctx.reporter.opoo(&format!("{name} not pinned"));
        }
    }

    drop(locks);
    Ok(())
}
