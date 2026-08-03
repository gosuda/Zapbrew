use std::collections::BTreeSet;
use std::fs;
use std::str::FromStr;

use camino::{Utf8Path, Utf8PathBuf};
use zapbrew_prefix::Keg;
use zapbrew_types::FormulaName;

use crate::install::format_size;
use crate::state::{InstalledFormula, InstalledState, scan};
use crate::transaction::{RemovalInput, RemovalKeg, acquire_formula_locks, remove_formulae};
use crate::{Ctx, OpError};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Args {
    pub names: Vec<String>,
    pub force: bool,
    pub ignore_dependencies: bool,
}

pub async fn run(ctx: &Ctx, args: Args) -> Result<(), OpError> {
    if args.names.is_empty() {
        return Err(OpError::Refusal {
            message: "No formulae specified for uninstall.".to_owned(),
        });
    }
    let names = canonical_names(&args.names)?;
    let locks = acquire_formula_locks(&ctx.env, &names)?;
    let state = scan(&ctx.env)?;
    preflight(&ctx.env.cellar, &state, &names, &args)?;
    remove_locked(ctx, &state, &names, args.force)?;
    drop(locks);
    if !ctx.env.no_autoremove {
        crate::autoremove::run(ctx, crate::autoremove::Args::default()).await?;
    }
    Ok(())
}

pub(crate) fn remove_locked(
    ctx: &Ctx,
    state: &InstalledState,
    names: &BTreeSet<String>,
    force: bool,
) -> Result<(), OpError> {
    let mut transactions = Vec::with_capacity(names.len());
    let mut remaining = Vec::new();
    let mut leftovers = Vec::new();

    for name in names {
        let installed = state.formula(name).ok_or_else(|| OpError::Refusal {
            message: format!("No such keg: {}/{name}", ctx.env.cellar),
        })?;
        let targets = targets(installed, force);
        for keg in &targets {
            ctx.reporter.print(&format!(
                "Uninstalling {}... ({})",
                keg.path(),
                format_size(keg.size())
            ));
        }
        let selected = targets
            .iter()
            .map(|keg| {
                Ok(RemovalKeg {
                    keg: Keg::new(
                        &ctx.env.cellar,
                        installed.name().clone(),
                        keg.version().clone(),
                    )?,
                    linked: keg.is_linked(),
                    optlinked: keg.is_optlinked(),
                })
            })
            .collect::<Result<Vec<_>, OpError>>()?;
        let remove_rack = selected.len() == installed.kegs().len();
        if !remove_rack {
            remaining.push((
                name.clone(),
                installed
                    .kegs()
                    .iter()
                    .filter(|keg| {
                        !targets
                            .iter()
                            .any(|target| target.version() == keg.version())
                    })
                    .map(|keg| keg.version().to_string())
                    .collect::<Vec<_>>(),
            ));
        }
        leftovers.push((name.clone(), configuration_paths(ctx, name)?));
        transactions.push(RemovalInput {
            name: installed.name().clone(),
            targets: selected,
            remove_rack,
        });
    }

    remove_formulae(ctx, transactions)?;

    for (name, versions) in remaining {
        let verb = if versions.len() == 1 { "is" } else { "are" };
        ctx.reporter.print(&format!(
            "{name} {} {verb} still installed.\nTo remove all versions, run:\n  brew uninstall --force {name}",
            to_sentence(&versions)
        ));
    }
    for (name, paths) in leftovers {
        if !paths.is_empty() {
            ctx.reporter.opoo(&format!(
                "The following {name} configuration files have not been removed!\nIf desired, remove them manually with `rm -rf`:\n  {}",
                paths
                    .iter()
                    .map(|path| path.as_str())
                    .collect::<Vec<_>>()
                    .join("\n  ")
            ));
        }
    }
    Ok(())
}

fn canonical_names(requested: &[String]) -> Result<BTreeSet<String>, OpError> {
    requested
        .iter()
        .map(|name| {
            FormulaName::from_str(name)
                .map(|name| name.name().to_owned())
                .map_err(|source| OpError::InvalidState {
                    reason: format!("formula name {name} is invalid: {source}"),
                })
        })
        .collect()
}

fn preflight(
    cellar: &Utf8Path,
    state: &InstalledState,
    names: &BTreeSet<String>,
    args: &Args,
) -> Result<(), OpError> {
    for name in names {
        if !state.contains(name) {
            return Err(OpError::Refusal {
                message: format!("No such keg: {cellar}/{name}"),
            });
        }
    }

    if !args.ignore_dependencies {
        let mut required = Vec::new();
        let mut dependents = BTreeSet::new();
        for name in names {
            let installed = state.formula(name).ok_or_else(|| OpError::InvalidState {
                reason: format!("installed formula {name} disappeared during preflight"),
            })?;
            let outside = state
                .dependents_of(name)
                .into_iter()
                .filter(|formula| !names.contains(formula.name().name()))
                .collect::<Vec<_>>();
            if !outside.is_empty() {
                required.extend(
                    targets(installed, args.force)
                        .into_iter()
                        .map(|keg| keg.path().to_string()),
                );
                dependents.extend(
                    outside
                        .into_iter()
                        .map(|formula| formula.name().name().to_owned()),
                );
            }
        }
        if !required.is_empty() {
            let required_verb = if required.len() == 1 {
                "it is"
            } else {
                "they are"
            };
            let dependent_verb = if dependents.len() == 1 { "is" } else { "are" };
            return Err(OpError::Refusal {
                message: format!(
                    "Refusing to uninstall {}\nbecause {required_verb} required by {}, which {dependent_verb} currently installed.\nYou can override this and force removal with:\n  brew uninstall --ignore-dependencies {}",
                    to_sentence(&required),
                    to_sentence(&dependents.into_iter().collect::<Vec<_>>()),
                    args.names.join(" ")
                ),
            });
        }
    }

    for name in names {
        let installed = state.formula(name).ok_or_else(|| OpError::InvalidState {
            reason: format!("installed formula {name} disappeared during pin preflight"),
        })?;
        if installed.pinned().is_some() {
            return Err(OpError::Refusal {
                message: format!("{name} is pinned. You must unpin it to uninstall."),
            });
        }
    }
    Ok(())
}

fn targets(installed: &InstalledFormula, force: bool) -> Vec<&crate::state::InstalledKeg> {
    if force {
        return installed.kegs().iter().collect();
    }
    installed
        .linked()
        .or_else(|| installed.optlinked())
        .or_else(|| installed.latest())
        .into_iter()
        .collect()
}

fn configuration_paths(ctx: &Ctx, name: &str) -> Result<Vec<Utf8PathBuf>, OpError> {
    let root = ctx.env.prefix.join("etc").join(name);
    let metadata = match fs::symlink_metadata(root.as_std_path()) {
        Ok(metadata) => metadata,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => return Err(OpError::io("inspect", root, source)),
    };
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Ok(vec![root]);
    }
    let mut paths = Vec::new();
    collect_paths(&root, &mut paths)?;
    paths.sort();
    paths.dedup();
    Ok(paths)
}

fn collect_paths(path: &Utf8Path, paths: &mut Vec<Utf8PathBuf>) -> Result<(), OpError> {
    let entries = fs::read_dir(path.as_std_path())
        .map_err(|source| OpError::io("read configuration directory", path, source))?;
    for entry in entries {
        let entry =
            entry.map_err(|source| OpError::io("read configuration entry", path, source))?;
        let child =
            Utf8PathBuf::from_path_buf(entry.path()).map_err(|path| OpError::InvalidState {
                reason: format!("configuration path is not UTF-8: {}", path.display()),
            })?;
        paths.push(child.clone());
        let metadata = fs::symlink_metadata(child.as_std_path())
            .map_err(|source| OpError::io("inspect", child.clone(), source))?;
        if metadata.is_dir() && !metadata.file_type().is_symlink() {
            collect_paths(&child, paths)?;
        }
    }
    Ok(())
}

fn to_sentence(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [only] => only.clone(),
        [left, right] => format!("{left} and {right}"),
        _ => {
            let Some((last, rest)) = items.split_last() else {
                return String::new();
            };
            format!("{}, and {last}", rest.join(", "))
        }
    }
}
