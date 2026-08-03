use std::collections::BTreeSet;
use std::fs;
use std::str::FromStr;

use camino::{Utf8Path, Utf8PathBuf};
use zapbrew_api::{Catalog, Formula};
use zapbrew_pour::{LinkOptions, LinkReport, link, unlink};
use zapbrew_prefix::{Keg, Prefix};
use zapbrew_types::{Arch, BottleTag, FormulaName};

use crate::state::{InstalledFormula, InstalledKeg, scan_selected};
use crate::transaction::acquire_formula_locks;
use crate::{Ctx, OpError};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Args {
    pub names: Vec<String>,
    pub overwrite: bool,
    pub dry_run: bool,
    pub force: bool,
}

pub async fn run(ctx: &Ctx, args: Args) -> Result<(), OpError> {
    let names = resolved_names(ctx, &args.names)?;
    let mut locked_names = names.clone();
    for name in &names {
        let formula = ctx
            .catalog
            .get(name)
            .ok_or_else(|| OpError::MissingFormula { name: name.clone() })?;
        locked_names.extend(overwrite_sibling_names(&ctx.catalog, formula));
    }

    let locks = acquire_formula_locks(&ctx.env, &locked_names)?;
    let state = scan_selected(&ctx.env, &locked_names)?;
    let prefix = Prefix::new(ctx.env.clone());

    for name in &names {
        let formula = ctx
            .catalog
            .get(name)
            .ok_or_else(|| OpError::MissingFormula { name: name.clone() })?;
        link_one(ctx, &prefix, &state, formula, &args)?;
    }

    drop(locks);
    Ok(())
}

fn link_one(
    ctx: &Ctx,
    prefix: &Prefix,
    state: &crate::state::InstalledState,
    formula: &Formula,
    args: &Args,
) -> Result<(), OpError> {
    let name = &formula.name;
    let installed = state.formula(name).ok_or_else(|| OpError::Refusal {
        message: format!("No such keg: {}/{name}", ctx.env.cellar),
    })?;
    let selected = selected_keg(installed).ok_or_else(|| OpError::Refusal {
        message: format!("No such keg: {}/{name}", ctx.env.cellar),
    })?;
    let keg = Keg::new(
        &ctx.env.cellar,
        installed.name().clone(),
        selected.version().clone(),
    )?;

    if let Some(linked) = installed.linked() {
        if linked.version() == selected.version() {
            ctx.reporter
                .opoo(&format!("Already linked: {}", keg.path()));
            let force = if formula.keg_only && !is_versioned(formula) {
                "--force "
            } else {
                ""
            };
            ctx.reporter.print(&format!(
                "To relink, run:\n  brew unlink {name} && brew link {force}{name}"
            ));
            return Ok(());
        }
        return Err(OpError::Refusal {
            message: format!(
                "Cannot link {name}\nAnother version is already linked: {}",
                linked.path()
            ),
        });
    }

    let versioned = is_versioned(formula);
    if !args.dry_run
        && formula.keg_only
        && is_default_macos_prefix(&ctx.env.prefix, ctx.env.bottle_tag)
        && is_by_macos(formula)
    {
        let explanation = formula
            .keg_only_reason
            .as_ref()
            .map(|reason| reason.explanation.trim())
            .unwrap_or_default();
        ctx.reporter.opoo(&macos_refusal_message(name, explanation));
        return Ok(());
    }

    if !args.dry_run && formula.keg_only && !args.force && !versioned {
        ctx.reporter.opoo(&format!(
            "{name} is keg-only and must be linked with `--force`."
        ));
        print_path_hint(ctx, &keg);
        return Ok(());
    }
    let options = LinkOptions {
        overwrite: args.overwrite,
        dry_run: true,
        force: args.force || versioned || args.dry_run,
        keg_only: formula.keg_only,
    };
    let preview = link(&keg, prefix, options)?;

    if args.dry_run {
        reject_conflicts(ctx, formula, &preview, &[])?;
        ctx.reporter.print(if args.overwrite {
            "Would remove:"
        } else {
            "Would link:"
        });
        let paths = if args.overwrite {
            &preview.backups
        } else {
            &preview.linked
        };
        for path in paths {
            ctx.reporter.print(path.as_str());
        }
        if formula.keg_only && !versioned {
            print_path_hint(ctx, &keg);
        }
        return Ok(());
    }

    let siblings = linked_overwrite_siblings(ctx, state, formula)?;
    reject_conflicts(ctx, formula, &preview, &siblings)?;

    let mut unlinked = Vec::with_capacity(siblings.len());
    for sibling in &siblings {
        let report = unlink(sibling, prefix)?;
        ctx.reporter.print(&format!(
            "Unlinking {}... {} symlinks removed.",
            sibling.path(),
            report.removed.len()
        ));
        unlinked.push(sibling.clone());
    }

    let report = match link(
        &keg,
        prefix,
        LinkOptions {
            dry_run: false,
            ..options
        },
    ) {
        Ok(report) if report.conflicts.is_empty() => report,
        Ok(report) => {
            restore_links(prefix, &unlinked)?;
            reject_conflicts(ctx, formula, &report, &[])?;
            return Err(OpError::InvalidState {
                reason: "link conflict was not reported".to_owned(),
            });
        }
        Err(error) => {
            restore_links(prefix, &unlinked)?;
            return Err(error.into());
        }
    };

    ctx.reporter.print(&format!(
        "Linking {}... {} symlinks created.",
        keg.path(),
        report.linked.len() + 2
    ));
    if formula.keg_only && !versioned {
        print_path_hint(ctx, &keg);
    }
    Ok(())
}

pub(crate) fn canonical_names(
    requested: &[String],
    operation: &str,
) -> Result<BTreeSet<String>, OpError> {
    if requested.is_empty() {
        return Err(OpError::Refusal {
            message: format!("No formulae specified for {operation}."),
        });
    }
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

pub(crate) fn selected_keg(installed: &InstalledFormula) -> Option<&InstalledKeg> {
    installed.kegs().iter().max_by(|left, right| {
        left.tab()
            .source
            .versions
            .version_scheme
            .cmp(&right.tab().source.versions.version_scheme)
            .then_with(|| left.version().cmp(right.version()))
    })
}

fn resolved_names(ctx: &Ctx, requested: &[String]) -> Result<BTreeSet<String>, OpError> {
    let validated = canonical_names(requested, "link")?;
    validated
        .into_iter()
        .map(|name| {
            ctx.catalog
                .get(&name)
                .map(|formula| formula.name.clone())
                .ok_or(OpError::MissingFormula { name })
        })
        .collect()
}

fn overwrite_sibling_names(catalog: &Catalog, formula: &Formula) -> BTreeSet<String> {
    let base = unversioned_name(&formula.name);
    catalog
        .iter()
        .filter(|other| other.name != formula.name && unversioned_name(&other.name) == base)
        .map(|other| other.name.clone())
        .collect()
}

fn linked_overwrite_siblings(
    ctx: &Ctx,
    state: &crate::state::InstalledState,
    formula: &Formula,
) -> Result<Vec<Keg>, OpError> {
    let mut siblings = Vec::new();
    for name in overwrite_sibling_names(&ctx.catalog, formula) {
        let Some(installed) = state.formula(&name) else {
            continue;
        };
        let Some(linked) = installed.linked() else {
            continue;
        };
        let sibling_formula = ctx.catalog.get(&name);
        if !formula.keg_only && !sibling_formula.is_some_and(|item| item.keg_only) {
            continue;
        }
        siblings.push(Keg::new(
            &ctx.env.cellar,
            installed.name().clone(),
            linked.version().clone(),
        )?);
    }
    siblings.sort_by(|left, right| left.name().name().cmp(right.name().name()));
    Ok(siblings)
}

fn restore_links(prefix: &Prefix, kegs: &[Keg]) -> Result<(), OpError> {
    for keg in kegs.iter().rev() {
        let report = link(
            keg,
            prefix,
            LinkOptions {
                force: true,
                ..LinkOptions::default()
            },
        )?;
        if !report.conflicts.is_empty() {
            return Err(OpError::InvalidState {
                reason: format!("failed to restore links for {}", keg.path()),
            });
        }
    }
    Ok(())
}

fn reject_conflicts(
    ctx: &Ctx,
    formula: &Formula,
    report: &LinkReport,
    siblings: &[Keg],
) -> Result<(), OpError> {
    let conflict = report
        .conflicts
        .iter()
        .find(|path| !siblings.iter().any(|keg| resolves_inside(path, keg.path())));
    let Some(dst) = conflict else {
        return Ok(());
    };
    let rel = dst.strip_prefix(&ctx.env.prefix).unwrap_or(dst);
    let suggestion = conflict_suggestion(ctx, dst);
    Err(OpError::Refusal {
        message: format!(
            "Could not symlink {rel}\nTarget {dst} {suggestion}\nTo force the link and overwrite all conflicting files:\n  brew link --overwrite {}\n\nTo list all files that would be deleted:\n  brew link --overwrite {} --dry-run",
            formula.name, formula.name
        ),
    })
}

fn conflict_suggestion(ctx: &Ctx, dst: &Utf8Path) -> String {
    if let Some(owner) = symlink_owner(dst, &ctx.env.cellar) {
        format!("is a symlink belonging to {owner}. You can unlink it:\n  brew unlink {owner}")
    } else {
        format!("already exists. You may want to remove it:\n  rm '{dst}'")
    }
}

fn symlink_owner(dst: &Utf8Path, cellar: &Utf8Path) -> Option<String> {
    let metadata = fs::symlink_metadata(dst.as_std_path()).ok()?;
    if !metadata.file_type().is_symlink() {
        return None;
    }
    let target = Utf8PathBuf::from_path_buf(fs::canonicalize(dst.as_std_path()).ok()?).ok()?;
    let cellar = Utf8PathBuf::from_path_buf(fs::canonicalize(cellar.as_std_path()).ok()?).ok()?;
    target
        .strip_prefix(cellar)
        .ok()?
        .components()
        .next()
        .map(|component| component.as_str().to_owned())
}

fn resolves_inside(path: &Utf8Path, root: &Utf8Path) -> bool {
    let Some(path) = fs::canonicalize(path.as_std_path())
        .ok()
        .and_then(|path| Utf8PathBuf::from_path_buf(path).ok())
    else {
        return false;
    };
    let Some(root) = fs::canonicalize(root.as_std_path())
        .ok()
        .and_then(|path| Utf8PathBuf::from_path_buf(path).ok())
    else {
        return false;
    };
    path.starts_with(root)
}

fn unversioned_name(name: &str) -> &str {
    name.split_once('@').map_or(name, |(base, _)| base)
}

fn reason(formula: &Formula) -> Option<&str> {
    formula
        .keg_only_reason
        .as_ref()
        .map(|reason| reason.reason.trim_start_matches(':'))
}

fn is_versioned(formula: &Formula) -> bool {
    reason(formula) == Some("versioned_formula")
}

fn is_by_macos(formula: &Formula) -> bool {
    matches!(
        reason(formula),
        Some("provided_by_macos" | "shadowed_by_macos")
    )
}

fn is_default_macos_prefix(prefix: &Utf8Path, tag: BottleTag) -> bool {
    match tag {
        BottleTag::MacOs {
            arch: Arch::Arm64, ..
        } => prefix == Utf8Path::new("/opt/homebrew"),
        BottleTag::MacOs {
            arch: Arch::X86_64, ..
        } => prefix == Utf8Path::new("/usr/local"),
        _ => false,
    }
}

fn macos_refusal_message(name: &str, explanation: &str) -> String {
    if explanation.is_empty() {
        format!("Refusing to link macOS provided/shadowed software: {name}")
    } else {
        format!("Refusing to link macOS provided/shadowed software: {name}\n{explanation}")
    }
}

fn print_path_hint(ctx: &Ctx, keg: &Keg) {
    let profile = ctx.env.home.join(".profile");
    let mut commands = Vec::new();
    for dir in ["bin", "sbin"] {
        if keg.path().join(dir).is_dir() {
            commands.push(format!(
                "  echo 'export PATH=\"{}/opt/{}/{dir}:$PATH\"' >> {profile}",
                ctx.env.prefix,
                keg.name().name()
            ));
        }
    }
    if !commands.is_empty() {
        ctx.reporter.print(&format!(
            "\nIf you need to have this software first in your PATH instead consider running:\n{}",
            commands.join("\n")
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::{is_default_macos_prefix, macos_refusal_message};
    use camino::Utf8Path;
    use zapbrew_types::BottleTag;

    #[test]
    fn default_macos_prefixes_and_refusal_body_are_exact() {
        let arm = BottleTag::from_host("macos", "arm64", Some("tahoe")).expect("arm macOS tag");
        let intel =
            BottleTag::from_host("macos", "x86_64", Some("tahoe")).expect("Intel macOS tag");
        assert!(is_default_macos_prefix(Utf8Path::new("/opt/homebrew"), arm));
        assert!(is_default_macos_prefix(Utf8Path::new("/usr/local"), intel));
        assert!(!is_default_macos_prefix(Utf8Path::new("/scratch"), arm));
        assert_eq!(
            macos_refusal_message("foo", "macOS already provides it."),
            "Refusing to link macOS provided/shadowed software: foo\nmacOS already provides it."
        );
    }
}
