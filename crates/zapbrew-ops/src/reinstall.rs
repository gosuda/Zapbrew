use std::collections::BTreeSet;
use std::str::FromStr;

use zapbrew_api::Formula;
use zapbrew_net::{DownloadRequest, download_all, select_bottle};
use zapbrew_types::FormulaName;

use crate::install::{make_tab, replacement, resolve_formula};
use crate::state::scan_selected;
use crate::transaction::{InstallInput, acquire_formula_locks, install as install_transaction};
use crate::{Ctx, OpError};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Args {
    pub names: Vec<String>,
}

pub async fn run(ctx: &Ctx, args: Args) -> Result<(), OpError> {
    let mut formulae = Vec::new();
    let mut affected = BTreeSet::new();
    for requested in &args.names {
        let formula = resolve_formula(ctx, requested).await?;
        if affected.insert(formula.name.clone()) {
            formulae.push(formula);
        }
    }

    let _locks = acquire_formula_locks(&ctx.env, &affected)?;
    let state = scan_selected(&ctx.env, &affected)?;
    let mut plans = Vec::with_capacity(formulae.len());
    for formula in formulae {
        let installed = state
            .formula(&formula.name)
            .ok_or_else(|| OpError::Refusal {
                message: format!("{} is not installed", formula.name),
            })?;
        if installed.pinned().is_some() {
            return Err(OpError::Refusal {
                message: format!(
                    "{} is pinned. You must unpin it to reinstall.",
                    formula.full_name
                ),
            });
        }
        let old = installed
            .kegs()
            .iter()
            .find(|keg| keg.version() == &formula.pkg_version)
            .or_else(|| installed.linked())
            .or_else(|| installed.latest())
            .ok_or_else(|| OpError::Refusal {
                message: format!("{} is not installed", formula.name),
            })?;
        plans.push((
            formula,
            old.tab().installed_on_request,
            request_for(ctx, formula)?,
        ));
    }

    for (_, _, request) in &plans {
        ctx.reporter
            .ohai(&format!("Fetching {}", request.name.as_str()));
        ctx.reporter
            .oh1(&format!("Downloading {}", request.bottle.url));
    }
    let cached = download_all(
        &ctx.env,
        &ctx.http,
        plans
            .iter()
            .map(|(_, _, request)| request.clone())
            .collect(),
    )
    .await?;

    for ((formula, installed_on_request, request), cached) in plans.into_iter().zip(cached) {
        let file_name = cached
            .alias
            .file_name()
            .or_else(|| cached.path.file_name())
            .unwrap_or(cached.path.as_str());
        ctx.reporter.ohai(&format!("Pouring {file_name}"));
        let installed = state.formula(&formula.name);
        let summary = install_transaction(
            ctx,
            InstallInput {
                formula,
                bottle: &request.bottle,
                cached: &cached,
                tab: make_tab(ctx, formula, installed_on_request)?,
                replacement: replacement(ctx, formula, installed)?,
            },
        )?;
        if installed_on_request
            && let Some(caveats) = formula.caveats.as_deref()
            && !caveats.is_empty()
        {
            ctx.reporter.ohai("Caveats");
            ctx.reporter.print(caveats);
        }
        ctx.reporter.print(&format!(
            "{}  {}: {} files, {}B",
            ctx.env.install_badge, summary.keg, summary.files, summary.size
        ));
    }
    Ok(())
}

fn request_for(ctx: &Ctx, formula: &Formula) -> Result<DownloadRequest, OpError> {
    let name = FormulaName::from_str(&formula.name).map_err(|source| OpError::InvalidState {
        reason: format!("catalog formula name {} is invalid: {source}", formula.name),
    })?;
    let bottle = formula
        .bottle
        .as_ref()
        .ok_or_else(|| zapbrew_net::NetError::NoBottle {
            name: formula.name.clone(),
            tag: ctx.env.bottle_tag,
        })?;
    Ok(DownloadRequest {
        bottle: select_bottle(&ctx.env, &name, &bottle.files)?.clone(),
        name,
        pkg_version: formula.pkg_version.clone(),
        rebuild: bottle.rebuild,
    })
}
