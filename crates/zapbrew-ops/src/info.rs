use std::collections::BTreeSet;
use std::str::FromStr;

use serde::Serialize;
use serde_json::Value;
use zapbrew_api::{Dependency, DependencyTag, Formula};
use zapbrew_types::FormulaName;

use crate::install::{format_size, substitute_prefixes};
use crate::state::{InstalledFormula, InstalledKeg, scan_selected};
use crate::{Ctx, OpError};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Args {
    pub names: Vec<String>,
    pub json_v2: bool,
}

pub async fn run(ctx: &Ctx, args: Args) -> Result<(), OpError> {
    let formulae = if args.names.is_empty() {
        if !args.json_v2 {
            return Err(OpError::Refusal {
                message: "this command requires a formula or cask argument".to_owned(),
            });
        }
        ctx.catalog.iter().collect::<Vec<_>>()
    } else {
        args.names
            .iter()
            .map(|requested| {
                ctx.catalog
                    .get(requested)
                    .ok_or_else(|| OpError::MissingFormula {
                        name: requested.clone(),
                    })
            })
            .collect::<Result<Vec<_>, _>>()?
    };

    if args.json_v2 {
        ctx.reporter.print(&json_v2(&formulae)?);
        return Ok(());
    }

    let selected = formulae
        .iter()
        .map(|formula| formula.name.clone())
        .collect::<BTreeSet<_>>();
    let state = scan_selected(&ctx.env, &selected)?;

    for (index, formula) in formulae.into_iter().enumerate() {
        if index > 0 {
            ctx.reporter.print("");
        }
        render_formula(ctx, formula, state.formula(&formula.name));
    }
    Ok(())
}

fn render_formula(ctx: &Ctx, formula: &Formula, installed: Option<&InstalledFormula>) {
    let mut title = format!(
        "{}: stable {}",
        formula.full_name, formula.pkg_version.version
    );
    if host_bottle_available(ctx, formula) {
        title.push_str(" (bottled)");
    }
    if formula.keg_only {
        title.push_str(" [keg-only]");
    }
    ctx.reporter.ohai(&title);

    if let Some(description) = &formula.desc {
        ctx.reporter.print(description);
    }
    if let Some(homepage) = &formula.homepage {
        ctx.reporter.print(homepage);
    }
    if !formula.aliases.is_empty() {
        ctx.reporter
            .print(&format!("Aliases: {}", formula.aliases.join(", ")));
    }
    if !formula.oldnames.is_empty() {
        ctx.reporter
            .print(&format!("Old Names: {}", formula.oldnames.join(", ")));
    }

    match installed.and_then(intent_keg) {
        Some(keg) if keg.tab().installed_on_request => {
            ctx.reporter.print("Installed (on request)");
        }
        Some(_) => ctx.reporter.print("Installed (as dependency)"),
        None => ctx.reporter.print("Not installed"),
    }

    if let Some(url) = github_url(formula) {
        ctx.reporter.print(&format!("From: {url}"));
    }
    if let Some(license) = &formula.license {
        ctx.reporter.print(&format!("License: {license}"));
    }

    if let Some(installed) = installed {
        render_installed(ctx, formula, installed);
    }
    render_dependencies(ctx, &formula.dependencies);

    if !ctx.reporter.is_quiet()
        && let Some(caveats) = formula.caveats.as_deref().filter(|text| !text.is_empty())
    {
        ctx.reporter.ohai("Caveats");
        ctx.reporter.print(&substitute_prefixes(ctx, caveats));
    }
}

/// Keg whose receipt reports install intent, with Homebrew `Tab.for_formula`
/// precedence: opt-linked, then linked, then the sole installed keg, then latest.
fn intent_keg(installed: &InstalledFormula) -> Option<&InstalledKeg> {
    installed
        .optlinked()
        .or_else(|| installed.linked())
        .or_else(|| match installed.kegs() {
            [keg] => Some(keg),
            _ => None,
        })
        .or_else(|| installed.latest())
}

fn render_installed(ctx: &Ctx, formula: &Formula, installed: &InstalledFormula) {
    let Some(latest) = installed.latest() else {
        return;
    };
    let kegs = installed
        .kegs()
        .iter()
        .rev()
        .filter(|keg| keg.path() == latest.path() || keg.is_linked())
        .collect::<Vec<_>>();
    if kegs.is_empty() {
        return;
    }

    ctx.reporter.ohai("Installed Versions");
    let version_width = kegs
        .iter()
        .map(|keg| keg.version().to_string().len())
        .max()
        .unwrap_or_default();
    let sizes = kegs
        .iter()
        .map(|keg| format!("({})", abv(keg)))
        .collect::<Vec<_>>();
    let size_width = sizes.iter().map(String::len).max().unwrap_or_default();

    for (keg, size) in kegs.into_iter().zip(sizes) {
        let version = keg.version().to_string();
        let linked = if keg.is_linked() { " [Linked]" } else { "" };
        let size = if keg.is_linked() {
            format!("{size:<size_width$}")
        } else {
            size
        };
        ctx.reporter.print(&format!(
            "{} {version:<version_width$} {size}{linked}",
            formula.full_name
        ));
    }
}

fn abv(keg: &InstalledKeg) -> String {
    format!("{} files, {}", keg.file_count(), format_size(keg.size()))
}

fn render_dependencies(ctx: &Ctx, dependencies: &[Dependency]) {
    let groups = [
        ("Build", DependencyGroup::Build),
        ("Required", DependencyGroup::Required),
        ("Recommended", DependencyGroup::Recommended),
        ("Optional", DependencyGroup::Optional),
    ];
    let mut lines = Vec::new();
    for (label, group) in groups {
        let names = dependencies
            .iter()
            .filter(|dependency| group.matches(dependency))
            .map(|dependency| dependency.name.as_str())
            .collect::<Vec<_>>();
        if !names.is_empty() {
            lines.push(format!("{label} ({}): {}", names.len(), names.join(", ")));
        }
    }
    if lines.is_empty() {
        return;
    }
    ctx.reporter.ohai("Dependencies");
    for line in lines {
        ctx.reporter.print(&line);
    }
}

#[derive(Clone, Copy)]
enum DependencyGroup {
    Build,
    Required,
    Recommended,
    Optional,
}

impl DependencyGroup {
    fn matches(self, dependency: &Dependency) -> bool {
        match self {
            Self::Build => dependency.has(DependencyTag::Build),
            Self::Required => dependency.tags.is_empty(),
            Self::Recommended => dependency.has(DependencyTag::Recommended),
            Self::Optional => dependency.has(DependencyTag::Optional),
        }
    }
}

fn host_bottle_available(ctx: &Ctx, formula: &Formula) -> bool {
    let Some(bottle) = &formula.bottle else {
        return false;
    };
    let Ok(name) = FormulaName::from_str(&formula.name) else {
        return false;
    };
    zapbrew_net::select_bottle(&ctx.env, &name, &bottle.files).is_ok()
}

fn github_url(formula: &Formula) -> Option<String> {
    let tap = formula.tap.as_deref()?;
    let path = formula.ruby_source_path.as_deref()?;
    let (owner, repo) = tap.split_once('/')?;
    let owner = if owner.eq_ignore_ascii_case("homebrew") {
        "Homebrew"
    } else {
        owner
    };
    let repository = if repo.starts_with("homebrew-") {
        repo.to_owned()
    } else {
        format!("homebrew-{repo}")
    };
    Some(format!(
        "https://github.com/{owner}/{repository}/blob/HEAD/{path}"
    ))
}

#[derive(Serialize)]
struct JsonV2<'a> {
    formulae: Vec<&'a Value>,
    casks: [(); 0],
}

fn json_v2(formulae: &[&Formula]) -> Result<String, OpError> {
    let payload = JsonV2 {
        formulae: formulae.iter().map(|formula| &formula.raw).collect(),
        casks: [],
    };
    serde_json::to_string_pretty(&payload).map_err(|source| OpError::InvalidState {
        reason: format!("serialize info JSON: {source}"),
    })
}
