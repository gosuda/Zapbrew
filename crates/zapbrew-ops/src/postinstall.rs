use std::collections::BTreeSet;
use std::str::FromStr;

use zapbrew_types::FormulaName;

use crate::install_steps::InstallSteps;
use crate::state::{InstalledFormula, scan_selected};
use crate::{Ctx, OpError};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Args {
    pub names: Vec<String>,
}

pub async fn run(ctx: &Ctx, args: Args) -> Result<(), OpError> {
    let locked_names: BTreeSet<String> = args
        .names
        .iter()
        .map(|name| {
            FormulaName::from_str(name)
                .map(|n| n.name().to_owned())
                .map_err(|source| OpError::InvalidState {
                    reason: format!("{name}: {source}"),
                })
        })
        .collect::<Result<_, _>>()?;

    let state = scan_selected(&ctx.env, &locked_names)?;
    for name in &locked_names {
        let installed = state.formula(name).ok_or_else(|| OpError::Refusal {
            message: format!("No such keg: {}/{name}", ctx.env.cellar),
        })?;
        let keg = select_keg(installed)?;
        let formula = ctx
            .catalog
            .get(name)
            .ok_or_else(|| OpError::MissingFormula { name: name.clone() })?;

        if !formula.post_install_defined {
            ctx.reporter
                .opoo(&format!("{name}: no post-install method was defined."));
            continue;
        }

        ctx.reporter
            .ohai(&format!("Postinstalling {}", formula.full_name));

        let mut journal = crate::install_steps::StepJournal::default();
        let steps = InstallSteps::parse(ctx, formula)?;
        let keg_path = keg.path().to_path_buf();
        if let Err(err) = steps.execute(ctx, formula, &keg_path, &mut journal) {
            let leftovers = journal.rollback(&ctx.env);
            if leftovers.is_empty() {
                return Err(err);
            }
            return Err(OpError::RollbackIncomplete {
                original: Box::new(err),
                leftovers: leftovers
                    .iter()
                    .map(|p| p.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
            });
        }
        if let Some(root) = journal.cleanup_path().map(|p| p.to_path_buf()) {
            let remove = crate::install_steps::remove_tree_confined(&ctx.env, &root);
            journal.clear();
            if remove.is_err() && std::fs::symlink_metadata(root.as_std_path()).is_ok() {
                return Err(OpError::CleanupIncomplete {
                    keg: keg_path,
                    leftovers: vec![root],
                });
            }
        } else {
            journal.clear();
        }
    }

    Ok(())
}

fn select_keg(installed: &InstalledFormula) -> Result<&crate::state::InstalledKeg, OpError> {
    installed
        .linked()
        .or_else(|| installed.optlinked())
        .or_else(|| installed.latest())
        .ok_or_else(|| OpError::InvalidState {
            reason: format!("{} has no kegs to post-install", installed.name()),
        })
}
