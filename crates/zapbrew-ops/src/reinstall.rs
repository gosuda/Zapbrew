use camino::Utf8PathBuf;
use std::collections::BTreeSet;
use std::str::FromStr;
use zapbrew_api::Formula;
use zapbrew_net::{DownloadRequest, download_all, select_bottle};
use zapbrew_prefix::{LockGuard, Tab};
use zapbrew_types::FormulaName;

use crate::install::{format_size, make_tab, replacement, resolve_formula, substitute_prefixes};
use crate::install_steps::InstallSteps;
use crate::state::scan_selected;
use crate::tap::formula_taps;
use crate::transaction::{
    InstallInput, Replacement, acquire_formula_locks, acquire_shared_tap_locks,
    install as install_transaction,
};
use crate::{Ctx, OpError};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Args {
    pub names: Vec<String>,
    pub formula: bool,
    pub cask: bool,
    pub appdir: Option<Utf8PathBuf>,
}

/// One fully-preflighted formula reinstall.
struct FormulaPlan<'a> {
    formula: &'a Formula,
    steps: InstallSteps,
    installed_on_request: bool,
    tab: Tab,
    replacement: Replacement,
    request: DownloadRequest,
}

/// Formula preflight plus the locks that protect it through execution.
struct PreparedFormulas<'a> {
    _tap_locks: Vec<LockGuard>,
    _formula_locks: Vec<LockGuard>,
    plans: Vec<FormulaPlan<'a>>,
}

pub async fn run(ctx: &Ctx, args: Args) -> Result<(), OpError> {
    let (formulae, cask_tokens) = resolve_names(ctx, &args).await?;

    if args.appdir.is_some() && !formulae.is_empty() {
        return Err(OpError::Refusal {
            message: "zapbrew cannot honor --appdir with a formula: \
                     the formula reinstall path does not support it. Use brew."
                .to_owned(),
        });
    }

    let appdir = args.appdir.as_deref();

    // Cask preflight acquires token locks and inspects every installed record
    // before any formula or cask network or mutation.
    let cask_prepared = if !cask_tokens.is_empty() {
        Some(crate::cask::reinstall::preflight(
            ctx,
            &cask_tokens,
            appdir,
        )?)
    } else {
        None
    };

    let formula_prepared = if !formulae.is_empty() {
        Some(prepare_formulas(ctx, &formulae)?)
    } else {
        None
    };

    // Execute formulae first to preserve existing formula-only output ordering.
    if let Some(prepared) = formula_prepared {
        execute_formulas(ctx, prepared).await?;
    }

    if let Some(locked) = cask_prepared {
        locked.execute(ctx).await?;
    }

    Ok(())
}

/// Resolve every requested name into a formula or a cask token, honoring the
/// user's discriminator and formula-first auto mode. This is catalog-only: no
/// network, no file system, no mutation.
async fn resolve_names<'a>(
    ctx: &'a Ctx,
    args: &Args,
) -> Result<(Vec<&'a Formula>, Vec<String>), OpError> {
    let mut formulae = Vec::with_capacity(args.names.len());
    let mut cask_tokens = Vec::with_capacity(args.names.len());
    let mut seen_formulae = BTreeSet::new();
    let mut seen_casks = BTreeSet::new();

    for requested in &args.names {
        match (args.formula, args.cask) {
            (true, false) => {
                let formula = resolve_formula(ctx, requested).await?;
                if seen_formulae.insert(formula.name.clone()) {
                    formulae.push(formula);
                }
            }
            (false, true) => {
                let cask = crate::cask::resolve(ctx, requested)?;
                if seen_casks.insert(cask.token.clone()) {
                    cask_tokens.push(cask.token.clone());
                }
            }
            (false, false) => {
                let formula = match ctx.catalog.resolve(requested) {
                    zapbrew_api::Resolution::Exact => ctx.catalog.get(requested),
                    zapbrew_api::Resolution::Alias { ref real } => ctx.catalog.get(real),
                    zapbrew_api::Resolution::Oldname { ref new } => ctx.catalog.get(new),
                    zapbrew_api::Resolution::Missing { .. } => None,
                };
                if let Some(formula) = formula {
                    if seen_formulae.insert(formula.name.clone()) {
                        formulae.push(formula);
                    }
                    continue;
                }

                if let Some(cask) = ctx.casks.get(requested) {
                    if seen_casks.insert(cask.token.clone()) {
                        cask_tokens.push(cask.token.clone());
                    }
                    continue;
                }

                let formula = resolve_formula(ctx, requested).await?;
                if seen_formulae.insert(formula.name.clone()) {
                    formulae.push(formula);
                }
            }
            (true, true) => unreachable!(),
        }
    }

    Ok((formulae, cask_tokens))
}

/// Pure preflight for a set of formulae: locks, scan, tab, replacement, and
/// bottle request. No network.
fn prepare_formulas<'a>(
    ctx: &'a Ctx,
    formulae: &[&'a Formula],
) -> Result<PreparedFormulas<'a>, OpError> {
    // brew's perform_preinstall_checks: refresh `<prefix>/lib/ld.so` for
    // relocated Linux bottles on a fresh prefix.
    zapbrew_prefix::symlink_ld_so(&ctx.env)?;
    zapbrew_prefix::setup_preferred_gcc_libs(&ctx.env)?;

    let mut affected = BTreeSet::new();
    let mut formula_steps = Vec::with_capacity(formulae.len());
    for formula in formulae {
        if affected.insert(formula.name.clone()) {
            formula_steps.push((*formula, InstallSteps::parse(ctx, formula)?));
        }
    }

    // Acquire shared tap locks before formula locks to prevent lock-order
    // inversion with untap's exclusive tap locks. Held through receipt commit.
    let taps = formula_taps(formula_steps.iter().map(|(f, _)| *f))?;
    let _tap_locks = acquire_shared_tap_locks(&ctx.env, &taps)?;
    let _formula_locks = acquire_formula_locks(&ctx.env, &affected)?;
    let state = scan_selected(&ctx.env, &affected)?;

    let mut plans = Vec::with_capacity(formula_steps.len());
    for (formula, steps) in formula_steps {
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
        let installed_on_request = old.tab().installed_on_request;
        plans.push(FormulaPlan {
            formula,
            steps,
            installed_on_request,
            tab: make_tab(ctx, formula, installed_on_request)?,
            replacement: replacement(ctx, formula, Some(installed))?,
            request: request_for(ctx, formula)?,
        });
    }

    Ok(PreparedFormulas {
        _tap_locks,
        _formula_locks,
        plans,
    })
}

async fn execute_formulas(ctx: &Ctx, prepared: PreparedFormulas<'_>) -> Result<(), OpError> {
    for plan in &prepared.plans {
        ctx.reporter
            .ohai(&format!("Fetching {}", plan.request.name.as_str()));
        ctx.reporter
            .oh1(&format!("Downloading {}", plan.request.bottle.url));
    }

    let requests: Vec<DownloadRequest> = prepared
        .plans
        .iter()
        .map(|plan| plan.request.clone())
        .collect();
    let cached = download_all(&ctx.env, &ctx.http, requests).await?;

    for (plan, cached) in prepared.plans.into_iter().zip(cached) {
        let file_name = cached
            .alias
            .file_name()
            .or_else(|| cached.path.file_name())
            .unwrap_or(cached.path.as_str());
        ctx.reporter.ohai(&format!("Pouring {file_name}"));
        let summary = install_transaction(
            ctx,
            InstallInput {
                formula: plan.formula,
                bottle: &plan.request.bottle,
                cached: &cached,
                tab: plan.tab,
                replacement: plan.replacement,
                steps: &plan.steps,
            },
        )?;
        if plan.installed_on_request
            && !ctx.reporter.is_quiet()
            && let Some(caveats) = plan.formula.caveats.as_deref()
            && !caveats.is_empty()
        {
            ctx.reporter.ohai("Caveats");
            ctx.reporter.print(&substitute_prefixes(ctx, caveats));
        }
        ctx.reporter.print(&format!(
            "{}  {}: {} files, {}",
            ctx.env.install_badge,
            summary.keg,
            summary.files,
            format_size(summary.size)
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
