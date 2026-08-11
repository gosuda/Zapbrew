use std::collections::BTreeSet;
use std::str::FromStr;

use zapbrew_api::{Cask, Formula};
use zapbrew_types::FormulaName;

use crate::state::{InstalledCask, InstalledFormula, scan, scan_casks, scan_selected};
use crate::{Ctx, OpError};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Args {
    pub names: Vec<String>,
    pub verbose: bool,
    pub json_v2: bool,
    pub greedy: bool,
    pub greedy_latest: bool,
    pub greedy_auto_updates: bool,
}

enum Target {
    Formula(String),
    Cask(String),
}

pub async fn run(ctx: &Ctx, args: Args) -> Result<(), OpError> {
    let cask_state = scan_casks(&ctx.env)?;

    let (formula_state, targets) = if args.names.is_empty() {
        let formula_state = scan(&ctx.env)?;
        let mut targets = Vec::new();
        for installed in formula_state.iter() {
            targets.push(Target::Formula(installed.name().name().to_owned()));
        }
        for installed in cask_state.iter() {
            targets.push(Target::Cask(installed.token().to_owned()));
        }
        (formula_state, targets)
    } else {
        let mut formula_selected = BTreeSet::new();
        let mut targets = Vec::new();
        for requested in &args.names {
            if let Some(formula) = ctx.catalog.get(requested) {
                formula_selected.insert(formula.name.clone());
                targets.push(Target::Formula(formula.name.clone()));
            } else if let Some(cask) = ctx.casks.get(requested) {
                targets.push(Target::Cask(cask.token.clone()));
            } else if cask_state.cask(requested).is_some() {
                targets.push(Target::Cask(requested.clone()));
            } else if FormulaName::from_str(requested).is_ok() {
                formula_selected.insert(requested.clone());
                targets.push(Target::Formula(requested.clone()));
            } else {
                return Err(OpError::Refusal {
                    message: format!("{requested} is not installed"),
                });
            }
        }
        let formula_state = scan_selected(&ctx.env, &formula_selected)?;
        (formula_state, targets)
    };

    let mut outdated: Vec<Outdated> = Vec::new();
    for target in targets {
        match target {
            Target::Formula(name) => {
                let Some(installed) = formula_state.formula(&name) else {
                    return Err(OpError::Refusal {
                        message: format!("{name} is not installed"),
                    });
                };
                let Some(formula) = ctx.catalog.get(&name) else {
                    ctx.reporter.opoo(&format!(
                        "{name} is installed but unavailable in the formula API; skipping."
                    ));
                    continue;
                };
                if is_outdated(formula, installed) {
                    outdated.push(Outdated::Formula(formula, installed));
                }
            }
            Target::Cask(token) => {
                let Some(installed) = cask_state.cask(&token) else {
                    return Err(OpError::Refusal {
                        message: format!("{token} is not installed"),
                    });
                };
                let Some(cask) = ctx.casks.get(&token) else {
                    ctx.reporter.opoo(&format!(
                        "{token} is installed but unavailable in the cask API; skipping."
                    ));
                    continue;
                };
                if is_outdated_cask(cask, installed, &args).is_some() {
                    outdated.push(Outdated::Cask(cask, installed));
                }
            }
        }
    }

    outdated.sort_by(|left, right| left.name().cmp(right.name()));

    if args.json_v2 {
        ctx.reporter.print(&json_v2(&outdated)?);
    } else if args.verbose {
        for entry in outdated {
            match entry {
                Outdated::Formula(formula, installed) => {
                    let versions = installed_versions(installed).join(", ");
                    let pinned = installed
                        .pinned()
                        .map(|keg| format!(" [pinned at {}]", keg.version()))
                        .unwrap_or_default();
                    ctx.reporter.print(&format!(
                        "{} ({versions}) < {}{pinned}",
                        formula.full_name, formula.pkg_version
                    ));
                }
                Outdated::Cask(cask, installed) => {
                    let version = installed.installed_version().unwrap_or("latest");
                    let current = cask.version.as_deref().unwrap_or("latest");
                    ctx.reporter
                        .print(&format!("{} ({version}) != {}", cask.token, current));
                }
            }
        }
    } else {
        for entry in outdated {
            match entry {
                Outdated::Formula(formula, _) => ctx.reporter.print(&formula.full_name),
                Outdated::Cask(cask, _) => ctx.reporter.print(&cask.token),
            }
        }
    }
    Ok(())
}

enum Outdated<'a> {
    Formula(&'a Formula, &'a InstalledFormula),
    Cask(&'a Cask, &'a InstalledCask),
}

impl Outdated<'_> {
    fn name(&self) -> &str {
        match self {
            Outdated::Formula(formula, _) => &formula.full_name,
            Outdated::Cask(cask, _) => &cask.token,
        }
    }
}

pub(crate) fn is_outdated(formula: &Formula, installed: &InstalledFormula) -> bool {
    let kegs = installed.kegs();
    if kegs.is_empty() {
        return false;
    }
    let scheme_bumped_for_all = kegs.iter().all(|keg| {
        formula.version_scheme > keg.tab().source.versions.version_scheme
            && formula.pkg_version != *keg.version()
    });
    let pkg_newer_than_max = kegs
        .iter()
        .map(|keg| keg.version())
        .max()
        .is_some_and(|version| formula.pkg_version > *version);
    scheme_bumped_for_all || pkg_newer_than_max
}

pub(crate) fn is_outdated_cask(
    cask: &Cask,
    installed: &InstalledCask,
    args: &Args,
) -> Option<String> {
    let version = cask.version.as_deref()?;
    let installed_version = installed.installed_version()?;
    if version == "latest" {
        return if args.greedy || args.greedy_latest {
            Some(installed_version.to_owned())
        } else {
            None
        };
    }
    if installed_version == version {
        return None;
    }
    if cask.auto_updates && !args.greedy && !args.greedy_auto_updates {
        return None;
    }
    Some(installed_version.to_owned())
}

fn installed_versions(installed: &InstalledFormula) -> Vec<String> {
    installed
        .kegs()
        .iter()
        .map(|keg| keg.version().to_string())
        .collect()
}

fn json_v2(entries: &[Outdated<'_>]) -> Result<String, OpError> {
    let mut formulae = Vec::with_capacity(entries.len());
    let mut casks = Vec::with_capacity(entries.len());
    for entry in entries {
        match entry {
            Outdated::Formula(formula, installed) => {
                let pinned_version = installed.pinned().map(|keg| keg.version().to_string());
                formulae.push(format!(
                    concat!(
                        "    {{\n",
                        "      \"name\": {},\n",
                        "      \"installed_versions\": {},\n",
                        "      \"current_version\": {},\n",
                        "      \"pinned\": {},\n",
                        "      \"pinned_version\": {}\n",
                        "    }}"
                    ),
                    serde_json::to_string(&formula.full_name).map_err(json_error)?,
                    serde_json::to_string(&installed_versions(installed)).map_err(json_error)?,
                    serde_json::to_string(&formula.pkg_version.to_string()).map_err(json_error)?,
                    pinned_version.is_some(),
                    serde_json::to_string(&pinned_version).map_err(json_error)?,
                ));
            }
            Outdated::Cask(cask, installed) => {
                let installed_version = installed.installed_version().unwrap_or("latest");
                let current_version = cask.version.clone().unwrap_or_else(|| "latest".to_owned());
                casks.push(format!(
                    concat!(
                        "    {{\n",
                        "      \"name\": {},\n",
                        "      \"installed_versions\": {},\n",
                        "      \"current_version\": {},\n",
                        "      \"pinned\": {},\n",
                        "      \"pinned_version\": {}\n",
                        "    }}"
                    ),
                    serde_json::to_string(&cask.token).map_err(json_error)?,
                    serde_json::to_string(&[installed_version]).map_err(json_error)?,
                    serde_json::to_string(&current_version).map_err(json_error)?,
                    false,
                    serde_json::to_string(&Option::<String>::None).map_err(json_error)?,
                ));
            }
        }
    }
    let formula_body = if formulae.is_empty() {
        String::new()
    } else {
        format!("\n{}\n  ", formulae.join(",\n"))
    };
    let cask_body = if casks.is_empty() {
        String::new()
    } else {
        format!("\n{}\n  ", casks.join(",\n"))
    };
    Ok(format!(
        "{{\n  \"formulae\": [{formula_body}],\n  \"casks\": [{cask_body}]\n}}"
    ))
}

fn json_error(source: serde_json::Error) -> OpError {
    OpError::InvalidState {
        reason: format!("serialize outdated JSON: {source}"),
    }
}
