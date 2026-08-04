use std::collections::{BTreeSet, HashSet};
use std::str::FromStr;
use std::time::{SystemTime, UNIX_EPOCH};

use zapbrew_api::{Formula, Resolution};
use zapbrew_net::{DownloadRequest, download_all, select_bottle};
use zapbrew_prefix::{Keg, RuntimeDependency, Source, SourceVersions, Tab};
use zapbrew_types::{Arch, BottleTag, FormulaName};

use crate::dependency::{DependencyMode, DependencyOptions, EdgeFilter, expand};
use crate::install_steps::InstallSteps;
use crate::state::{InstalledFormula, InstalledKeg, InstalledState, scan_selected};
use crate::transaction::{
    InstallInput, Replacement, acquire_formula_locks, install as install_transaction,
};
use crate::{Ctx, OpError};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Args {
    pub names: Vec<String>,
    pub only_dependencies: bool,
    pub force: bool,
    pub dry_run: bool,
    pub build_from_source: bool,
    pub head: bool,
    pub interactive: bool,
}

struct Candidate<'a> {
    formula: &'a Formula,
    requested: bool,
    steps: InstallSteps,
}

pub async fn run(ctx: &Ctx, args: Args) -> Result<(), OpError> {
    refuse_ruby_modes(&args)?;

    let roots = resolve_roots(ctx, &args.names).await?;
    let mut candidates = dependency_candidates(ctx, &roots)?;
    if !args.only_dependencies {
        candidates.extend(
            roots
                .iter()
                .map(|formula| {
                    Ok(Candidate {
                        formula,
                        requested: true,
                        steps: InstallSteps::parse(ctx, formula)?,
                    })
                })
                .collect::<Result<Vec<_>, OpError>>()?,
        );
    }
    deduplicate_candidates(&mut candidates);
    for candidate in &candidates {
        validate_formula(ctx, candidate.formula)?;
    }

    let affected = affected_names(ctx, &candidates);
    let state;
    let _locks;
    if args.dry_run {
        state = scan_selected(&ctx.env, &affected)?;
    } else {
        // brew's perform_preinstall_checks: refresh `<prefix>/lib/ld.so` so a
        // fresh Linux prefix can run relocated bottles whose interpreter
        // points there. Never mutates under `--dry-run`.
        zapbrew_prefix::symlink_ld_so(&ctx.env)?;
        _locks = acquire_formula_locks(&ctx.env, &affected)?;
        state = scan_selected(&ctx.env, &affected)?;
    }

    let selected = select_unmet(ctx, candidates, &state, args.force)?;
    check_conflicts(ctx, &selected, &state, args.force)?;

    if args.dry_run {
        if !selected.is_empty() {
            ctx.reporter.ohai("Would install");
            ctx.reporter.print(
                &selected
                    .iter()
                    .map(|candidate| candidate.formula.name.as_str())
                    .collect::<Vec<_>>()
                    .join(" "),
            );
        }
        return Ok(());
    }
    let bottles = bottle_requests(ctx, selected)?;

    for (_, _, request) in &bottles {
        ctx.reporter
            .ohai(&format!("Fetching {}", request.name.as_str()));
        if ctx.reporter.is_verbose() {
            ctx.reporter
                .oh1(&format!("Downloading {}", request.bottle.url));
        }
    }
    let downloaded = download_all(
        &ctx.env,
        &ctx.http,
        bottles
            .iter()
            .map(|(_, _, request)| request.clone())
            .collect(),
    )
    .await?;

    for ((candidate, bottle, _), cached) in bottles.into_iter().zip(downloaded) {
        let file_name = cached
            .alias
            .file_name()
            .or_else(|| cached.path.file_name())
            .unwrap_or(cached.path.as_str());
        ctx.reporter.ohai(&format!("Pouring {file_name}"));

        let installed = state.formula(&candidate.formula.name);
        let replacement = replacement(ctx, candidate.formula, installed)?;
        let installed_on_request = candidate.requested
            || installed
                .and_then(InstalledFormula::latest)
                .is_some_and(|keg| keg.tab().installed_on_request);
        let tab = make_tab(ctx, candidate.formula, installed_on_request)?;
        let summary = install_transaction(
            ctx,
            InstallInput {
                formula: candidate.formula,
                bottle,
                cached: &cached,
                tab,
                replacement,
                steps: &candidate.steps,
            },
        )?;

        if candidate.formula.keg_only {
            let reason = candidate
                .formula
                .keg_only_reason
                .as_ref()
                .map(|reason| {
                    if reason.explanation.is_empty() {
                        reason.reason.as_str()
                    } else {
                        reason.explanation.as_str()
                    }
                })
                .unwrap_or("it is keg-only");
            ctx.reporter.opoo(&format!(
                "{} is keg-only, which means it was not symlinked into {},\nbecause {reason}.",
                candidate.formula.name, ctx.env.prefix
            ));
        }
        if candidate.formula.post_install_defined {
            ctx.reporter.opoo(&format!(
                "{} defines post_install; run brew postinstall {} to execute it.",
                candidate.formula.name, candidate.formula.name
            ));
        }
        if candidate.requested
            && !ctx.reporter.is_quiet()
            && let Some(caveats) = candidate.formula.caveats.as_deref()
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

pub(crate) async fn resolve_formula<'a>(
    ctx: &'a Ctx,
    requested: &str,
) -> Result<&'a Formula, OpError> {
    match ctx.catalog.resolve(requested) {
        Resolution::Exact => ctx.catalog.get(requested),
        Resolution::Alias { ref real } => ctx.catalog.get(real),
        Resolution::Oldname { ref new } => ctx.catalog.get(new),
        Resolution::Missing { .. } => {
            if let Some(tap) = ctx
                .catalog
                .migration_hint(&ctx.env, &ctx.http, requested)
                .await?
            {
                return Err(OpError::Refusal {
                    message: format!("{requested} was migrated to {tap}"),
                });
            }
            None
        }
    }
    .ok_or_else(|| OpError::MissingFormula {
        name: requested.to_owned(),
    })
}

pub(crate) fn make_tab(
    ctx: &Ctx,
    formula: &Formula,
    installed_on_request: bool,
) -> Result<Tab, OpError> {
    let dependencies = expand(
        ctx.catalog.as_ref(),
        [&formula.name],
        &DependencyOptions {
            target: ctx.env.bottle_tag,
            mode: DependencyMode::Pour,
            filter: EdgeFilter::ALL,
        },
    )?;
    let direct: HashSet<String> = formula
        .dependencies
        .iter()
        .filter(|dependency| !dependency.is_build() && !dependency.is_test())
        .filter_map(|dependency| ctx.catalog.get(&dependency.name))
        .map(|dependency| dependency.name.clone())
        .chain(
            formula
                .uses_from_macos
                .iter()
                .filter_map(|dependency| ctx.catalog.get(&dependency.name))
                .map(|dependency| dependency.name.clone()),
        )
        .collect();
    let runtime_dependencies = dependencies
        .into_iter()
        .map(|dependency| {
            let formula =
                ctx.catalog
                    .get(&dependency.name)
                    .ok_or_else(|| OpError::MissingFormula {
                        name: dependency.name.clone(),
                    })?;
            Ok(RuntimeDependency {
                full_name: formula.full_name.clone(),
                version: formula.pkg_version.version.to_string(),
                revision: formula.revision,
                bottle_rebuild: None,
                pkg_version: formula.pkg_version.to_string(),
                declared_directly: direct.contains(&formula.name),
                compatibility_version: None,
            })
        })
        .collect::<Result<Vec<_>, OpError>>()?;
    let arch = match ctx.env.bottle_tag {
        BottleTag::Linux { arch } | BottleTag::MacOs { arch, .. } => Some(match arch {
            Arch::X86_64 => "x86_64".to_owned(),
            Arch::Arm64 => "arm64".to_owned(),
        }),
        BottleTag::All => None,
    };
    let time = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|duration| i64::try_from(duration.as_secs()).ok());

    Ok(Tab {
        // Homebrew-compatible version string matching the reference tree
        // (6.0.14). Must be >= 5.1.15: real brew's Linux
        // `BottleSpecification#skip_relocation?` gates on
        // `tab.parsed_homebrew_version >= "5.1.15"` when reading receipts.
        homebrew_version: Some("6.0.14-zapbrew".to_owned()),
        built_as_bottle: true,
        poured_from_bottle: true,
        loaded_from_api: true,
        loaded_from_internal_api: true,
        installed_on_request,
        time,
        aliases: formula.aliases.clone(),
        runtime_dependencies: Some(runtime_dependencies),
        source: Source {
            path: formula.ruby_source_path.clone(),
            tap: formula.tap.clone(),
            tap_git_head: formula.tap_git_head.clone(),
            versions: SourceVersions {
                stable: Some(formula.pkg_version.version.to_string()),
                version_scheme: formula.version_scheme,
                ..SourceVersions::default()
            },
            ..Source::default()
        },
        arch,
        ..Tab::default()
    })
}

pub(crate) fn replacement(
    ctx: &Ctx,
    formula: &Formula,
    installed: Option<&InstalledFormula>,
) -> Result<Replacement, OpError> {
    let name = formula_name(formula)?;
    let linked = installed
        .and_then(InstalledFormula::linked)
        .map(|keg| installed_keg(ctx, &name, keg))
        .transpose()?;
    let target = installed
        .and_then(|formula_state| {
            formula_state
                .kegs()
                .iter()
                .find(|keg| keg.version() == &formula.pkg_version)
        })
        .map(|keg| installed_keg(ctx, &name, keg))
        .transpose()?;
    Ok(Replacement { linked, target })
}

pub(crate) fn linked_replacement(
    ctx: &Ctx,
    formula: &Formula,
    installed: &InstalledFormula,
) -> Result<Replacement, OpError> {
    let name = formula_name(formula)?;
    let linked = installed
        .linked()
        .map(|keg| installed_keg(ctx, &name, keg))
        .transpose()?;
    Ok(Replacement {
        linked,
        target: None,
    })
}

fn formula_name(formula: &Formula) -> Result<FormulaName, OpError> {
    FormulaName::from_str(&formula.name).map_err(|source| OpError::InvalidState {
        reason: format!("catalog formula name {} is invalid: {source}", formula.name),
    })
}

fn installed_keg(ctx: &Ctx, name: &FormulaName, keg: &InstalledKeg) -> Result<Keg, OpError> {
    Keg::new(&ctx.env.cellar, name.clone(), keg.version().clone()).map_err(OpError::from)
}

fn refuse_ruby_modes(args: &Args) -> Result<(), OpError> {
    if args.build_from_source {
        return Err(OpError::Refusal {
            message: "zapbrew cannot build from source: formulae are Ruby definitions. Use bottles (default) or brew."
                .to_owned(),
        });
    }
    if args.head {
        return Err(OpError::Refusal {
            message: "zapbrew cannot install HEAD formulae: formulae are Ruby definitions. Use bottled stable releases or brew."
                .to_owned(),
        });
    }
    if args.interactive {
        return Err(OpError::Refusal {
            message: "zapbrew cannot install interactively: formulae are Ruby definitions. Use bottles (default) or brew."
                .to_owned(),
        });
    }
    Ok(())
}

async fn resolve_roots<'a>(ctx: &'a Ctx, names: &[String]) -> Result<Vec<&'a Formula>, OpError> {
    let mut roots = Vec::new();
    let mut seen = HashSet::new();
    for name in names {
        let formula = resolve_formula(ctx, name).await?;
        if seen.insert(formula.name.clone()) {
            roots.push(formula);
        }
    }
    Ok(roots)
}

fn dependency_candidates<'a>(
    ctx: &'a Ctx,
    roots: &[&Formula],
) -> Result<Vec<Candidate<'a>>, OpError> {
    let dependencies = expand(
        ctx.catalog.as_ref(),
        roots.iter().map(|formula| formula.name.as_str()),
        &DependencyOptions {
            target: ctx.env.bottle_tag,
            mode: DependencyMode::Pour,
            filter: EdgeFilter::ALL,
        },
    )?;
    dependencies
        .into_iter()
        .map(|dependency| {
            ctx.catalog
                .get(&dependency.name)
                .ok_or(OpError::MissingFormula {
                    name: dependency.name,
                })
                .and_then(|formula| {
                    Ok(Candidate {
                        formula,
                        requested: false,
                        steps: InstallSteps::parse(ctx, formula)?,
                    })
                })
        })
        .collect()
}

fn deduplicate_candidates(candidates: &mut Vec<Candidate<'_>>) {
    let mut unique: Vec<Candidate<'_>> = Vec::with_capacity(candidates.len());
    for candidate in candidates.drain(..) {
        if let Some(index) = unique
            .iter()
            .position(|existing| existing.formula.name == candidate.formula.name)
        {
            unique[index].requested |= candidate.requested;
        } else {
            unique.push(candidate);
        }
    }
    *candidates = unique;
}

fn validate_formula(ctx: &Ctx, formula: &Formula) -> Result<(), OpError> {
    if formula.disabled {
        let reason = formula.disable_reason.as_deref().unwrap_or("is disabled");
        return Err(OpError::Refusal {
            message: format!("{} has been disabled because it {reason}!", formula.name),
        });
    }
    if formula.deprecated {
        let reason = formula
            .deprecation_reason
            .as_deref()
            .unwrap_or("is deprecated");
        ctx.reporter.opoo(&format!(
            "{} has been deprecated because it {reason}!",
            formula.name
        ));
    }
    Ok(())
}

fn affected_names(ctx: &Ctx, candidates: &[Candidate<'_>]) -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    for candidate in candidates {
        names.insert(candidate.formula.name.clone());
        for conflict in &candidate.formula.conflicts_with {
            names.insert(
                ctx.catalog
                    .get(&conflict.name)
                    .map_or_else(|| conflict.name.clone(), |formula| formula.name.clone()),
            );
        }
    }
    names
}

fn select_unmet<'a>(
    ctx: &Ctx,
    candidates: Vec<Candidate<'a>>,
    state: &InstalledState,
    force: bool,
) -> Result<Vec<Candidate<'a>>, OpError> {
    let mut selected = Vec::new();
    for candidate in candidates {
        let installed = state.formula(&candidate.formula.name);
        if let Some(pinned) = installed.and_then(InstalledFormula::pinned)
            && pinned.version() != &candidate.formula.pkg_version
        {
            return Err(OpError::Refusal {
                message: format!(
                    "{} is pinned at {} but {} is available.",
                    candidate.formula.name,
                    pinned.version(),
                    candidate.formula.pkg_version
                ),
            });
        }
        let current_and_linked = installed.is_some_and(|formula| {
            formula
                .kegs()
                .iter()
                .any(|keg| keg.version() == &candidate.formula.pkg_version && keg.is_linked())
        });
        if current_and_linked && !force {
            if candidate.requested {
                ctx.reporter.opoo(&format!(
                    "{} {} is already installed and up-to-date.\nTo reinstall {}, run:\n  {} reinstall {}",
                    candidate.formula.name,
                    candidate.formula.pkg_version,
                    candidate.formula.pkg_version,
                    ctx.reporter.hint_program(),
                    candidate.formula.name
                ));
            }
            continue;
        }
        selected.push(candidate);
    }
    Ok(selected)
}

fn check_conflicts(
    ctx: &Ctx,
    selected: &[Candidate<'_>],
    state: &InstalledState,
    force: bool,
) -> Result<(), OpError> {
    if force {
        return Ok(());
    }
    for candidate in selected {
        let conflicts: Vec<_> = candidate
            .formula
            .conflicts_with
            .iter()
            .filter(|conflict| {
                ctx.catalog
                    .get(&conflict.name)
                    .and_then(|formula| state.formula(&formula.name))
                    .and_then(InstalledFormula::linked)
                    .is_some()
            })
            .collect();
        if conflicts.is_empty() {
            continue;
        }
        let names = conflicts
            .iter()
            .map(|conflict| conflict.name.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        let details = conflicts
            .iter()
            .map(|conflict| match conflict.reason.as_deref() {
                Some(reason) => format!("  {}: because {reason}", conflict.name),
                None => format!("  {}", conflict.name),
            })
            .collect::<Vec<_>>()
            .join("\n");
        return Err(OpError::Refusal {
            message: format!(
                "Cannot install {} because conflicting formulae are installed.\n{details}\n\nPlease `brew unlink {names}` before continuing.\n\nUnlinking removes a formula's symlinks from {}. You can\nlink the formula again after the install finishes. You can `--force` this\ninstall, but the build may fail or cause obscure side effects in the\nresulting software.",
                candidate.formula.full_name, ctx.env.prefix
            ),
        });
    }
    Ok(())
}

type BottlePlan<'a> = (
    Candidate<'a>,
    &'a zapbrew_types::BottleFile,
    DownloadRequest,
);

fn bottle_requests<'a>(
    ctx: &Ctx,
    selected: Vec<Candidate<'a>>,
) -> Result<Vec<BottlePlan<'a>>, OpError> {
    selected
        .into_iter()
        .map(|candidate| {
            let (bottle, request) = request_for_formula(ctx, candidate.formula)?;
            Ok((candidate, bottle, request))
        })
        .collect()
}

pub(crate) fn request_for_formula<'a>(
    ctx: &Ctx,
    formula: &'a Formula,
) -> Result<(&'a zapbrew_types::BottleFile, DownloadRequest), OpError> {
    let name = formula_name(formula)?;
    let bottle_block = formula
        .bottle
        .as_ref()
        .ok_or_else(|| zapbrew_net::NetError::NoBottle {
            name: formula.name.clone(),
            tag: ctx.env.bottle_tag,
        })?;
    let bottle = select_bottle(&ctx.env, &name, &bottle_block.files)?;
    Ok((
        bottle,
        DownloadRequest {
            name,
            bottle: bottle.clone(),
            pkg_version: formula.pkg_version.clone(),
            rebuild: bottle_block.rebuild,
        },
    ))
}

pub(crate) fn substitute_prefixes(ctx: &Ctx, caveats: &str) -> String {
    caveats
        .replace("#{HOMEBREW_PREFIX}", ctx.env.prefix.as_str())
        .replace("$HOMEBREW_PREFIX", ctx.env.prefix.as_str())
        .replace("@@HOMEBREW_PREFIX@@", ctx.env.prefix.as_str())
}

pub(crate) fn format_size(bytes: u64) -> String {
    const UNITS: [(&str, u64); 4] = [
        ("GB", 1024 * 1024 * 1024),
        ("MB", 1024 * 1024),
        ("KB", 1024),
        ("B", 1),
    ];
    for (unit, divisor) in UNITS {
        if bytes >= divisor && divisor > 1 {
            return format!("{:.1}{unit}", bytes as f64 / divisor as f64);
        }
        if divisor == 1 {
            return format!("{bytes}{unit}");
        }
    }
    format!("{bytes}B")
}
