use std::collections::BTreeSet;
use std::str::FromStr;

use zapbrew_api::{Formula, Resolution};
use zapbrew_types::FormulaName;

use crate::state::{InstalledFormula, scan, scan_selected};
use crate::{Ctx, OpError};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Args {
    pub names: Vec<String>,
    pub verbose: bool,
    pub json_v2: bool,
}

pub async fn run(ctx: &Ctx, args: Args) -> Result<(), OpError> {
    let (state, names) = if args.names.is_empty() {
        let state = scan(&ctx.env)?;
        let names = state
            .iter()
            .map(|formula| formula.name().name().to_owned())
            .collect::<Vec<_>>();
        (state, names)
    } else {
        let mut names = Vec::new();
        let mut selected = BTreeSet::new();
        for requested in &args.names {
            let name = canonical_name(ctx, requested)?;
            if selected.insert(name.clone()) {
                names.push(name);
            }
        }
        let state = scan_selected(&ctx.env, &selected)?;
        for name in &names {
            if !state.contains(name) {
                return Err(OpError::Refusal {
                    message: format!("{name} is not installed"),
                });
            }
        }
        (state, names)
    };

    let mut outdated = Vec::new();
    for name in names {
        let Some(installed) = state.formula(&name) else {
            continue;
        };
        let Some(formula) = ctx.catalog.get(&name) else {
            ctx.reporter.opoo(&format!(
                "{name} is installed but unavailable in the formula API; skipping."
            ));
            continue;
        };
        if is_outdated(formula, installed) {
            outdated.push((formula, installed));
        }
    }
    outdated.sort_by(|(left, _), (right, _)| left.full_name.cmp(&right.full_name));

    if args.json_v2 {
        ctx.reporter.print(&json_v2(&outdated)?);
    } else if args.verbose {
        for (formula, installed) in outdated {
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
    } else {
        for (formula, _) in outdated {
            ctx.reporter.print(&formula.full_name);
        }
    }
    Ok(())
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

fn canonical_name(ctx: &Ctx, requested: &str) -> Result<String, OpError> {
    let catalog_name = match ctx.catalog.resolve(requested) {
        Resolution::Exact => ctx
            .catalog
            .get(requested)
            .map(|formula| formula.name.clone()),
        Resolution::Alias { ref real } => Some(real.clone()),
        Resolution::Oldname { ref new } => Some(new.clone()),
        Resolution::Missing { .. } => None,
    };
    if let Some(name) = catalog_name {
        return Ok(name);
    }
    FormulaName::from_str(requested)
        .map(|name| name.name().to_owned())
        .map_err(|source| OpError::InvalidState {
            reason: format!("formula name {requested} is invalid: {source}"),
        })
}

fn installed_versions(installed: &InstalledFormula) -> Vec<String> {
    installed
        .kegs()
        .iter()
        .map(|keg| keg.version().to_string())
        .collect()
}

fn json_v2(entries: &[(&Formula, &InstalledFormula)]) -> Result<String, OpError> {
    let mut formulae = Vec::with_capacity(entries.len());
    for (formula, installed) in entries {
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
    let body = if formulae.is_empty() {
        String::new()
    } else {
        format!("\n{}\n  ", formulae.join(",\n"))
    };
    Ok(format!(
        "{{\n  \"formulae\": [{body}],\n  \"casks\": []\n}}"
    ))
}

fn json_error(source: serde_json::Error) -> OpError {
    OpError::InvalidState {
        reason: format!("serialize outdated JSON: {source}"),
    }
}
