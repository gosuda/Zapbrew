use std::collections::{BTreeMap, BTreeSet};
use std::fs;

use zapbrew_prefix::{Rack, Tab};

use crate::state::{InstalledState, scan};
use crate::transaction::acquire_formula_locks;
use crate::{Ctx, OpError};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Args {
    pub dry_run: bool,
}

pub async fn run(ctx: &Ctx, args: Args) -> Result<(), OpError> {
    let names = Rack::all(&ctx.env.cellar)?
        .into_iter()
        .map(|rack| rack.name().to_owned())
        .collect::<BTreeSet<_>>();
    let _locks = if args.dry_run {
        None
    } else {
        Some(acquire_formula_locks(&ctx.env, &names)?)
    };
    let state = scan(&ctx.env)?;
    let requested = strict_request_state(&state)?;
    let removable = fixpoint(&state, &requested);
    if removable.is_empty() {
        return Ok(());
    }

    let noun = if removable.len() == 1 {
        "formula"
    } else {
        "formulae"
    };
    let verb = if args.dry_run {
        "Would autoremove"
    } else {
        "Autoremoving"
    };
    ctx.reporter
        .oh1(&format!("{verb} {} unneeded {noun}:", removable.len()));
    ctx.reporter.print(
        &removable
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .join("\n"),
    );
    if args.dry_run {
        return Ok(());
    }
    crate::uninstall::remove_locked(ctx, &state, &removable, true)
}

fn strict_request_state(state: &InstalledState) -> Result<BTreeMap<String, bool>, OpError> {
    let mut requested = BTreeMap::new();
    for formula in state.iter() {
        let mut installed_on_request = false;
        for keg in formula.kegs() {
            let receipt = keg.path().join("INSTALL_RECEIPT.json");
            let content = fs::read_to_string(receipt.as_std_path()).map_err(|source| {
                OpError::io("read autoremove receipt", receipt.clone(), source)
            })?;
            let tab =
                serde_json::from_str::<Tab>(&content).map_err(|source| OpError::InvalidState {
                    reason: format!("invalid autoremove receipt {receipt}: {source}"),
                })?;
            installed_on_request |= tab.installed_on_request;
        }
        requested.insert(formula.name().name().to_owned(), installed_on_request);
    }
    Ok(requested)
}

fn fixpoint(state: &InstalledState, requested: &BTreeMap<String, bool>) -> BTreeSet<String> {
    let mut removable = BTreeSet::new();
    loop {
        let additions = state
            .iter()
            .filter(|formula| {
                let name = formula.name().name();
                !removable.contains(name) && !requested.get(name).copied().unwrap_or(true)
            })
            .filter(|formula| {
                state
                    .dependents_of(formula.name().name())
                    .into_iter()
                    .all(|dependent| removable.contains(dependent.name().name()))
            })
            .map(|formula| formula.name().name().to_owned())
            .collect::<Vec<_>>();
        if additions.is_empty() {
            return removable;
        }
        removable.extend(additions);
    }
}
