use std::collections::BTreeSet;
use std::str::FromStr;

use zapbrew_api::Formula;
use zapbrew_net::{DownloadRequest, download_all};
use zapbrew_prefix::{Keg, Rack};
use zapbrew_types::{BottleFile, FormulaName};

use crate::install::{
    format_size, linked_replacement, make_tab, request_for_formula, resolve_formula,
    substitute_prefixes,
};
use crate::install_steps::InstallSteps;
use crate::outdated::is_outdated;
use crate::state::{InstalledFormula, scan_selected};
use crate::transaction::{
    InstallInput, acquire_formula_locks, cleanup_replaced_kegs, install as install_transaction,
};
use crate::{Ctx, OpError};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Args {
    pub names: Vec<String>,
    pub dry_run: bool,
}

struct UpgradePlan<'a> {
    formula: &'a Formula,
    steps: InstallSteps,
    installed_on_request: bool,
    old_display: String,
    old_kegs: Vec<Keg>,
    bottle: &'a BottleFile,
    request: DownloadRequest,
}

pub async fn run(ctx: &Ctx, args: Args) -> Result<(), OpError> {
    let named = !args.names.is_empty();
    let (names, formulae) = if named {
        resolve_named(ctx, &args.names).await?
    } else {
        enumerate_installed(ctx)?
    };
    let _locks = if args.dry_run {
        None
    } else {
        Some(acquire_formula_locks(&ctx.env, &names)?)
    };
    let state = scan_selected(&ctx.env, &names)?;

    let mut available = Vec::new();
    for formula in formulae {
        let Some(installed) = state.formula(&formula.name) else {
            if named {
                return Err(OpError::Refusal {
                    message: format!("{} is not installed", formula.name),
                });
            }
            continue;
        };
        available.push((formula, installed));
    }

    let pinned = available
        .iter()
        .filter(|(_, installed)| installed.pinned().is_some())
        .map(|(formula, _)| *formula)
        .collect::<Vec<_>>();
    if !pinned.is_empty() {
        let noun = if pinned.len() == 1 {
            "package"
        } else {
            "packages"
        };
        let header = format!("Not upgrading {} pinned {noun}:", pinned.len());
        let list = pinned
            .iter()
            .map(|formula| format!("{} {}", formula.full_name, formula.pkg_version))
            .collect::<Vec<_>>()
            .join(", ");
        if named {
            return Err(OpError::Refusal {
                message: format!("{header}\n{list}"),
            });
        }
        ctx.reporter.opoo(&header);
        ctx.reporter.print(&list);
    }

    let mut selected = Vec::new();
    for (formula, installed) in available {
        if installed.pinned().is_some() {
            continue;
        }
        if is_outdated(formula, installed) {
            selected.push((formula, installed));
        } else if named {
            ctx.reporter.opoo(&format!(
                "{} {} is already installed and up-to-date.\nTo reinstall {}, run:\n  zapbrew reinstall {}",
                formula.name, formula.pkg_version, formula.pkg_version, formula.name
            ));
        }
    }
    selected.sort_by(|(left, _), (right, _)| left.full_name.cmp(&right.full_name));

    if args.dry_run {
        print_upgrade_summary(ctx, "Would upgrade", &selected);
        return Ok(());
    }
    if selected.is_empty() {
        if !named {
            ctx.reporter.oh1("No packages to upgrade");
        }
        return Ok(());
    }

    print_upgrade_summary(ctx, "Upgrading", &selected);
    let plans = build_plans(ctx, &selected)?;
    for plan in &plans {
        ctx.reporter
            .ohai(&format!("Fetching {}", plan.request.name.as_str()));
        ctx.reporter
            .oh1(&format!("Downloading {}", plan.request.bottle.url));
    }
    let cached = download_all(
        &ctx.env,
        &ctx.http,
        plans.iter().map(|plan| plan.request.clone()).collect(),
    )
    .await?;

    for (plan, cached) in plans.into_iter().zip(cached) {
        ctx.reporter
            .ohai(&format!("Upgrading {}", plan.formula.full_name));
        ctx.reporter.print(&format!(
            "  {} -> {}",
            plan.old_display, plan.formula.pkg_version
        ));
        let file_name = cached
            .alias
            .file_name()
            .or_else(|| cached.path.file_name())
            .unwrap_or(cached.path.as_str());
        ctx.reporter.ohai(&format!("Pouring {file_name}"));
        let installed = state
            .formula(&plan.formula.name)
            .ok_or_else(|| OpError::InvalidState {
                reason: format!("locked installed state lost {}", plan.formula.name),
            })?;
        let summary = install_transaction(
            ctx,
            InstallInput {
                formula: plan.formula,
                bottle: plan.bottle,
                cached: &cached,
                tab: make_tab(ctx, plan.formula, plan.installed_on_request)?,
                replacement: linked_replacement(ctx, plan.formula, installed)?,
                steps: &plan.steps,
            },
        )?;
        if !ctx.env.no_install_cleanup
            && let Err(error) =
                cleanup_replaced_kegs(&ctx.env, &summary.keg, &plan.request.name, &plan.old_kegs)
        {
            ctx.reporter.opoo(&format!(
                "Cleanup incomplete after upgrading {}: {error}",
                plan.formula.full_name
            ));
        }
        if plan.installed_on_request
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

async fn resolve_named<'a>(
    ctx: &'a Ctx,
    requested: &[String],
) -> Result<(BTreeSet<String>, Vec<&'a Formula>), OpError> {
    let mut names = BTreeSet::new();
    let mut formulae = Vec::new();
    for name in requested {
        let formula = resolve_formula(ctx, name).await?;
        if names.insert(formula.name.clone()) {
            formulae.push(formula);
        }
    }
    Ok((names, formulae))
}

fn enumerate_installed(ctx: &Ctx) -> Result<(BTreeSet<String>, Vec<&Formula>), OpError> {
    let names = Rack::all(&ctx.env.cellar)?
        .into_iter()
        .map(|rack| rack.name().to_owned())
        .collect::<BTreeSet<_>>();
    let mut formulae = Vec::new();
    for name in &names {
        if let Some(formula) = ctx.catalog.get(name) {
            formulae.push(formula);
        } else {
            ctx.reporter.opoo(&format!(
                "{name} is installed but unavailable in the formula API; skipping."
            ));
        }
    }
    Ok((names, formulae))
}

fn build_plans<'a>(
    ctx: &Ctx,
    selected: &[(&'a Formula, &InstalledFormula)],
) -> Result<Vec<UpgradePlan<'a>>, OpError> {
    selected
        .iter()
        .map(|(formula, installed)| {
            let active = installed
                .linked()
                .or_else(|| installed.optlinked())
                .or_else(|| installed.latest())
                .ok_or_else(|| OpError::InvalidState {
                    reason: format!("installed formula {} has no kegs", formula.name),
                })?;
            let name =
                FormulaName::from_str(&formula.name).map_err(|source| OpError::InvalidState {
                    reason: format!("catalog formula name {} is invalid: {source}", formula.name),
                })?;
            let old_kegs = installed
                .kegs()
                .iter()
                .filter(|keg| keg.version() != &formula.pkg_version)
                .map(|keg| Keg::new(&ctx.env.cellar, name.clone(), keg.version().clone()))
                .collect::<Result<Vec<_>, _>>()?;
            let (bottle, request) = request_for_formula(ctx, formula)?;
            Ok(UpgradePlan {
                formula,
                steps: InstallSteps::parse(ctx, formula)?,
                installed_on_request: active.tab().installed_on_request,
                old_display: active.version().to_string(),
                old_kegs,
                bottle,
                request,
            })
        })
        .collect()
}

fn print_upgrade_summary(ctx: &Ctx, verb: &str, selected: &[(&Formula, &InstalledFormula)]) {
    if selected.is_empty() {
        return;
    }
    let noun = if selected.len() == 1 {
        "package"
    } else {
        "packages"
    };
    ctx.reporter
        .oh1(&format!("{verb} {} outdated {noun}:", selected.len()));
    for (formula, installed) in selected {
        let old = installed
            .linked()
            .or_else(|| installed.optlinked())
            .or_else(|| installed.latest())
            .map(|keg| keg.version().to_string())
            .unwrap_or_default();
        ctx.reporter.print(&format!(
            "{}  {old} -> {}",
            formula.name, formula.pkg_version
        ));
    }
}
