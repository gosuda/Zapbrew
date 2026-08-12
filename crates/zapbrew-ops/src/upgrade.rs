use std::collections::BTreeSet;
use std::str::FromStr;

use camino::Utf8PathBuf;
use zapbrew_api::{Cask, Formula, Resolution};
use zapbrew_net::{DownloadRequest, download_all};
use zapbrew_prefix::{Keg, Rack};
use zapbrew_types::{BottleFile, FormulaName};

use crate::cask::{self, reinstall};
use crate::install::{
    format_size, linked_replacement, make_tab, request_for_formula, resolve_formula,
    substitute_prefixes,
};
use crate::install_steps::InstallSteps;
use crate::outdated::{self, is_outdated, is_outdated_cask};
use crate::state::{InstalledCask, InstalledFormula, scan_casks, scan_selected};
use crate::tap::formula_taps;
use crate::transaction::{
    InstallInput, acquire_formula_locks, acquire_shared_tap_locks, cleanup_replaced_kegs,
    install as install_transaction,
};
use crate::{Ctx, OpError};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Mode {
    #[default]
    Auto,
    Formula,
    Cask,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Args {
    pub names: Vec<String>,
    pub dry_run: bool,
    pub mode: Mode,
    pub appdir: Option<Utf8PathBuf>,
    pub greedy: bool,
    pub greedy_latest: bool,
    pub greedy_auto_updates: bool,
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

struct SelectedCask<'a> {
    cask: &'a Cask,
    old_display: String,
}

struct ResolvedTargets<'a> {
    names: BTreeSet<String>,
    formulae: Vec<&'a Formula>,
    casks: Vec<(&'a Cask, &'a InstalledCask)>,
    unresolved: Vec<String>,
}

pub async fn run(ctx: &Ctx, args: Args) -> Result<(), OpError> {
    let named = !args.names.is_empty();
    let cask_state = scan_casks(&ctx.env)?;
    let ResolvedTargets {
        mut names,
        mut formulae,
        casks,
        unresolved,
    } = resolve_targets(ctx, &args, &cask_state).await?;

    let cask_outdated_args = outdated::Args {
        greedy: named || args.greedy,
        greedy_latest: args.greedy_latest,
        greedy_auto_updates: args.greedy_auto_updates,
        ..outdated::Args::default()
    };
    let mut selected_casks = Vec::new();
    for (cask, installed) in casks {
        if is_outdated_cask(cask, installed, &cask_outdated_args).is_some() {
            selected_casks.push(SelectedCask {
                cask,
                old_display: installed.installed_version().unwrap_or("latest").to_owned(),
            });
        }
    }
    selected_casks.sort_by(|left, right| left.cask.token.cmp(&right.cask.token));

    let cask_tokens = selected_casks
        .iter()
        .map(|selected| selected.cask.token.clone())
        .collect::<Vec<_>>();

    if !cask_tokens.is_empty() {
        reinstall::validate(
            ctx,
            &cask_tokens,
            args.appdir.as_deref(),
            reinstall::Purpose::Upgrade,
        )?;
    }
    for requested in unresolved {
        let formula = resolve_formula(ctx, &requested).await?;
        if names.insert(formula.name.clone()) {
            formulae.push(formula);
        }
    }
    if args.appdir.is_some() && !formulae.is_empty() {
        return Err(OpError::Refusal {
            message: "--appdir cannot be used when upgrading formulae.".to_owned(),
        });
    }

    let _tap_locks = if args.dry_run {
        None
    } else {
        // Acquire shared tap locks before formula locks to prevent lock-order
        // inversion with untap's exclusive tap locks. Held through receipt
        // commit (end of function).
        let taps = formula_taps(formulae.iter().copied())?;
        Some(acquire_shared_tap_locks(&ctx.env, &taps)?)
    };
    let _locks = if args.dry_run {
        None
    } else {
        Some(acquire_formula_locks(&ctx.env, &names)?)
    };
    let locked_casks = if args.dry_run || cask_tokens.is_empty() {
        None
    } else {
        let locked_tokens = reinstall::lock_tokens(ctx, &cask_tokens, args.appdir.as_deref())?;
        let current = scan_casks(&ctx.env)?;
        selected_casks = selected_casks
            .into_iter()
            .filter_map(|selected| {
                let installed = current.cask(&selected.cask.token)?;
                is_outdated_cask(selected.cask, installed, &cask_outdated_args).map(|_| {
                    SelectedCask {
                        cask: selected.cask,
                        old_display: installed.installed_version().unwrap_or("latest").to_owned(),
                    }
                })
            })
            .collect();
        let still_outdated = selected_casks
            .iter()
            .map(|selected| selected.cask.token.clone())
            .collect::<BTreeSet<_>>();
        let locked = locked_tokens.prepare(
            ctx,
            &still_outdated,
            args.appdir.as_deref(),
            reinstall::Purpose::Upgrade,
        )?;
        Some(locked.validate_downloads(ctx)?)
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
                "{} {} is already installed and up-to-date.\nTo reinstall {}, run:\n  {} reinstall {}",
                formula.name,
                formula.pkg_version,
                formula.pkg_version,
                ctx.reporter.hint_program(),
                formula.name
            ));
        }
    }
    selected.sort_by(|(left, _), (right, _)| left.full_name.cmp(&right.full_name));

    if args.dry_run {
        print_upgrade_summary(ctx, "Would upgrade", &selected);
        print_cask_summary(ctx, "Would upgrade", &selected_casks);
        return Ok(());
    }
    if selected.is_empty() && selected_casks.is_empty() {
        if !named {
            ctx.reporter.oh1("No packages to upgrade");
        }
        return Ok(());
    }
    if !selected.is_empty() {
        // brew's perform_preinstall_checks: refresh <prefix>/lib/ld.so and the
        // preferred GCC ldconfig snippet before pouring Linux bottles.
        zapbrew_prefix::symlink_ld_so(&ctx.env)?;
        zapbrew_prefix::setup_preferred_gcc_libs(&ctx.env)?;
    }

    let mut plans = Vec::new();
    for &(formula, installed) in &selected {
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
        plans.push(UpgradePlan {
            formula,
            steps: InstallSteps::parse(ctx, formula)?,
            installed_on_request: active.tab().installed_on_request,
            old_display: active.version().to_string(),
            old_kegs,
            bottle,
            request,
        });
    }

    print_upgrade_summary(ctx, "Upgrading", &selected);
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
    let downloaded_casks = match locked_casks {
        Some(casks) => Some(casks.download(ctx).await?),
        None => None,
    };

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

    if let Some(downloaded_casks) = downloaded_casks {
        print_cask_summary(ctx, "Upgrading", &selected_casks);
        downloaded_casks.execute(ctx).await?;
    }
    Ok(())
}

async fn resolve_targets<'a>(
    ctx: &'a Ctx,
    args: &Args,
    cask_state: &'a crate::state::InstalledCaskState,
) -> Result<ResolvedTargets<'a>, OpError> {
    if args.names.is_empty() {
        let (names, formulae) = if args.mode == Mode::Cask {
            (BTreeSet::new(), Vec::new())
        } else {
            enumerate_installed(ctx)?
        };
        let casks = if args.mode == Mode::Formula {
            Vec::new()
        } else {
            enumerate_installed_casks(ctx, cask_state)
        };
        return Ok(ResolvedTargets {
            names,
            formulae,
            casks,
            unresolved: Vec::new(),
        });
    }

    let mut names = BTreeSet::new();
    let mut formulae = Vec::new();
    let mut casks = Vec::new();
    let mut cask_tokens = BTreeSet::new();
    let mut unresolved = Vec::new();
    for requested in &args.names {
        match args.mode {
            Mode::Formula => {
                let formula = resolve_formula(ctx, requested).await?;
                if names.insert(formula.name.clone()) {
                    formulae.push(formula);
                }
            }
            Mode::Cask => {
                select_named_cask(ctx, cask_state, requested, &mut cask_tokens, &mut casks)?
            }
            Mode::Auto => {
                let formula = match ctx.catalog.resolve(requested) {
                    Resolution::Exact => ctx.catalog.get(requested),
                    Resolution::Alias { ref real } => ctx.catalog.get(real),
                    Resolution::Oldname { ref new } => ctx.catalog.get(new),
                    Resolution::Missing { .. } => None,
                };
                if let Some(formula) = formula {
                    if names.insert(formula.name.clone()) {
                        formulae.push(formula);
                    }
                } else if ctx.casks.get(requested).is_some() {
                    select_named_cask(ctx, cask_state, requested, &mut cask_tokens, &mut casks)?;
                } else {
                    unresolved.push(requested.clone());
                }
            }
        }
    }
    Ok(ResolvedTargets {
        names,
        formulae,
        casks,
        unresolved,
    })
}

fn select_named_cask<'a>(
    ctx: &'a Ctx,
    state: &'a crate::state::InstalledCaskState,
    requested: &str,
    seen: &mut BTreeSet<String>,
    selected: &mut Vec<(&'a Cask, &'a InstalledCask)>,
) -> Result<(), OpError> {
    let cask = cask::resolve(ctx, requested)?;
    let Some(installed) = state.cask(&cask.token) else {
        return Err(OpError::Refusal {
            message: format!("{} is not installed", cask.token),
        });
    };
    if seen.insert(cask.token.clone()) {
        selected.push((cask, installed));
    }
    Ok(())
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

fn enumerate_installed_casks<'a>(
    ctx: &'a Ctx,
    state: &'a crate::state::InstalledCaskState,
) -> Vec<(&'a Cask, &'a InstalledCask)> {
    let mut selected = Vec::new();
    for installed in state.iter() {
        if let Some(cask) = ctx.casks.get(installed.token()) {
            selected.push((cask, installed));
        } else {
            ctx.reporter.opoo(&format!(
                "{} is installed but unavailable in the cask API; skipping.",
                installed.token()
            ));
        }
    }
    selected
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

fn print_cask_summary(ctx: &Ctx, verb: &str, selected: &[SelectedCask<'_>]) {
    if selected.is_empty() {
        return;
    }
    let noun = if selected.len() == 1 { "cask" } else { "casks" };
    ctx.reporter
        .oh1(&format!("{verb} {} outdated {noun}:", selected.len()));
    for selected in selected {
        ctx.reporter.print(&format!(
            "{}  {} -> {}",
            selected.cask.token,
            selected.old_display,
            selected.cask.version.as_deref().unwrap_or("latest")
        ));
    }
}
