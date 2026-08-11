use std::collections::{BTreeSet, HashSet};
use std::fs;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::str::FromStr;

use serde::Serialize;
use serde_json::Value;
use zapbrew_api::{Cask, Dependency, DependencyTag, Formula};
use zapbrew_types::FormulaName;

use crate::install::{format_size, substitute_prefixes};
use crate::size::disk_usage_readable;
use crate::state::{InstalledCask, InstalledFormula, InstalledKeg, scan_casks, scan_selected};
use crate::{Ctx, OpError};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Args {
    pub names: Vec<String>,
    pub json_v2: bool,
}

enum Target<'a> {
    Formula(&'a Formula),
    Cask(&'a Cask),
}

pub async fn run(ctx: &Ctx, args: Args) -> Result<(), OpError> {
    if args.json_v2 {
        let (formulae, casks) = if args.names.is_empty() {
            (
                ctx.catalog.iter().collect::<Vec<_>>(),
                ctx.casks.iter().collect::<Vec<_>>(),
            )
        } else {
            let mut formulae = Vec::new();
            let mut casks = Vec::new();
            for requested in &args.names {
                if let Some(formula) = ctx.catalog.get(requested) {
                    formulae.push(formula);
                } else if let Some(cask) = ctx.casks.get(requested) {
                    casks.push(cask);
                } else {
                    return Err(OpError::MissingFormula {
                        name: requested.clone(),
                    });
                }
            }
            (formulae, casks)
        };
        ctx.reporter.print(&json_v2(&formulae, &casks)?);
        return Ok(());
    }

    if args.names.is_empty() {
        if !ctx.env.cellar.exists() {
            return Ok(());
        }

        let (rack_count, file_count, size) = cellar_statistics(&ctx.env.cellar)?;
        let abv = if file_count > 1 {
            format!(
                "{} files, {}",
                number_readable(file_count),
                disk_usage_readable(size)
            )
        } else {
            disk_usage_readable(size)
        };
        let keg = if rack_count == 1 { "keg" } else { "kegs" };
        ctx.reporter.print(&format!("{rack_count} {keg}, {abv}"));
        return Ok(());
    }

    let mut targets = Vec::new();
    let mut selected = BTreeSet::new();
    for requested in &args.names {
        if let Some(formula) = ctx.catalog.get(requested) {
            selected.insert(formula.name.clone());
            targets.push(Target::Formula(formula));
        } else if let Some(cask) = ctx.casks.get(requested) {
            targets.push(Target::Cask(cask));
        } else {
            return Err(OpError::MissingFormula {
                name: requested.clone(),
            });
        }
    }

    let state = scan_selected(&ctx.env, &selected)?;
    let cask_state = scan_casks(&ctx.env)?;

    for (index, target) in targets.iter().enumerate() {
        if index > 0 {
            ctx.reporter.print("");
        }
        match target {
            Target::Formula(formula) => {
                render_formula(ctx, formula, state.formula(&formula.name));
            }
            Target::Cask(cask) => {
                render_cask(ctx, cask, cask_state.cask(&cask.token))?;
            }
        }
    }
    Ok(())
}

fn number_readable(number: usize) -> String {
    let digits = number.to_string();
    let separators = digits.len().saturating_sub(1) / 3;
    let mut formatted = String::with_capacity(digits.len() + separators);
    for (index, digit) in digits.bytes().enumerate() {
        if index != 0 && (digits.len() - index).is_multiple_of(3) {
            formatted.push(',');
        }
        formatted.push(char::from(digit));
    }
    formatted
}

fn cellar_statistics(cellar: &camino::Utf8Path) -> Result<(usize, usize, u64), OpError> {
    let root = cellar.as_std_path();

    let mut rack_count = 0_usize;
    if fs::metadata(root).is_ok_and(|metadata| metadata.is_dir()) {
        let racks = fs::read_dir(root)
            .map_err(|source| OpError::io("read", cellar.to_path_buf(), source))?;
        for rack in racks {
            let rack = rack.map_err(|source| OpError::io("read", cellar.to_path_buf(), source))?;
            if rack.file_name().as_bytes().starts_with(b".") {
                continue;
            }
            let rack_path = rack.path();
            let metadata = fs::symlink_metadata(&rack_path)
                .map_err(|source| OpError::io("inspect", cellar.to_path_buf(), source))?;
            if !metadata.is_dir() {
                continue;
            }
            let versions = fs::read_dir(&rack_path)
                .map_err(|source| OpError::io("read", cellar.to_path_buf(), source))?;
            for version in versions {
                let version =
                    version.map_err(|source| OpError::io("read", cellar.to_path_buf(), source))?;
                match fs::metadata(version.path()) {
                    Ok(metadata) if metadata.is_dir() => {
                        rack_count = rack_count.saturating_add(1);
                        break;
                    }
                    Ok(_) | Err(_) => {}
                }
            }
        }
    }

    let usage_root = match fs::symlink_metadata(root) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            let target = fs::read_link(root)
                .map_err(|source| OpError::io("read symlink", cellar.to_path_buf(), source))?;
            if target.is_absolute() {
                target
            } else {
                match root.parent() {
                    Some(parent) => parent.join(target),
                    None => target,
                }
            }
        }
        Ok(_) => root.to_path_buf(),
        Err(source) => return Err(OpError::io("inspect", cellar.to_path_buf(), source)),
    };

    let usage_metadata = fs::metadata(&usage_root)
        .map_err(|source| OpError::io("inspect", cellar.to_path_buf(), source))?;
    if !usage_metadata.is_dir() {
        let metadata = fs::symlink_metadata(&usage_root)
            .map_err(|source| OpError::io("inspect", cellar.to_path_buf(), source))?;
        return Ok((rack_count, 1, metadata.len()));
    }

    let mut file_count = 0_usize;
    let mut size = 0_u64;
    let mut seen_files = HashSet::new();
    let mut pending = vec![usage_root];
    while let Some(directory) = pending.pop() {
        let metadata = fs::symlink_metadata(&directory)
            .map_err(|source| OpError::io("inspect", cellar.to_path_buf(), source))?;
        size = size.saturating_add(metadata.len());
        if metadata.file_type().is_symlink() {
            continue;
        }

        let entries = fs::read_dir(&directory)
            .map_err(|source| OpError::io("read", cellar.to_path_buf(), source))?;
        for entry in entries {
            let entry =
                entry.map_err(|source| OpError::io("read", cellar.to_path_buf(), source))?;
            let metadata = fs::symlink_metadata(entry.path())
                .map_err(|source| OpError::io("inspect", cellar.to_path_buf(), source))?;
            if metadata.is_dir() {
                pending.push(entry.path());
                continue;
            }
            if metadata.file_type().is_symlink()
                && fs::metadata(entry.path()).is_ok_and(|target| target.is_dir())
            {
                size = size.saturating_add(metadata.len());
                continue;
            }

            if entry.file_name() != ".DS_Store" {
                file_count = file_count.saturating_add(1);
            }
            if seen_files.insert((metadata.dev(), metadata.ino())) {
                size = size.saturating_add(metadata.len());
            }
        }
    }

    Ok((rack_count, file_count, size))
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::symlink;

    use camino::Utf8PathBuf;
    use tempfile::TempDir;

    use super::cellar_statistics;

    #[test]
    fn cellar_statistics_use_lstat_and_deduplicate_hardlink_bytes() {
        let temp = TempDir::new().expect("temp");
        let cellar =
            Utf8PathBuf::from_path_buf(temp.path().join("Cellar")).expect("UTF-8 temp path");
        let rack = cellar.join("sample");
        let version = rack.join("1.0");
        let bin = version.join("bin");
        std::fs::create_dir_all(&bin).expect("bin");
        let original = bin.join("sample");
        std::fs::write(&original, b"abc").expect("original");
        std::fs::hard_link(&original, bin.join("alias")).expect("hardlink");
        let link = bin.join("link");
        symlink(&original, &link).expect("symlink");
        let ds_store = bin.join(".DS_Store");
        std::fs::write(&ds_store, b"junk").expect("DS_Store");

        let directory_bytes = [&cellar, &rack, &version, &bin]
            .into_iter()
            .map(|path| {
                std::fs::symlink_metadata(path)
                    .expect("directory metadata")
                    .len()
            })
            .sum::<u64>();
        let expected_bytes = directory_bytes
            + std::fs::symlink_metadata(&original)
                .expect("original metadata")
                .len()
            + std::fs::symlink_metadata(&link)
                .expect("link metadata")
                .len()
            + std::fs::symlink_metadata(&ds_store)
                .expect("DS_Store metadata")
                .len();

        assert_eq!(
            cellar_statistics(&cellar).expect("statistics"),
            (1, 3, expected_bytes)
        );
    }

    #[test]
    fn cellar_statistics_resolve_root_and_follow_version_for_rack_count() {
        let temp = TempDir::new().expect("temp");
        let root =
            Utf8PathBuf::from_path_buf(temp.path().join("real-cellar")).expect("UTF-8 temp path");
        let rack = root.join("sample");
        let outside =
            Utf8PathBuf::from_path_buf(temp.path().join("outside-version")).expect("UTF-8 path");
        std::fs::create_dir_all(&rack).expect("rack");
        std::fs::create_dir_all(&outside).expect("outside version");
        let version_link = rack.join("1.0");
        symlink(&outside, &version_link).expect("version symlink");
        let cellar =
            Utf8PathBuf::from_path_buf(temp.path().join("Cellar")).expect("UTF-8 temp path");
        symlink(&root, &cellar).expect("Cellar symlink");

        let expected_bytes = [&root, &rack]
            .into_iter()
            .map(|path| {
                std::fs::symlink_metadata(path)
                    .expect("directory metadata")
                    .len()
            })
            .sum::<u64>()
            + std::fs::symlink_metadata(&version_link)
                .expect("version link metadata")
                .len();

        assert_eq!(
            cellar_statistics(&cellar).expect("statistics"),
            (1, 0, expected_bytes)
        );
    }

    #[test]
    fn cellar_statistics_resolve_one_root_symlink_hop() {
        let temp = TempDir::new().expect("temp");
        let root =
            Utf8PathBuf::from_path_buf(temp.path().join("real-cellar")).expect("UTF-8 temp path");
        std::fs::create_dir_all(root.join("sample").join("1.0")).expect("version");
        let alias =
            Utf8PathBuf::from_path_buf(temp.path().join("cellar-alias")).expect("UTF-8 temp path");
        symlink(&root, &alias).expect("alias symlink");
        let cellar =
            Utf8PathBuf::from_path_buf(temp.path().join("Cellar")).expect("UTF-8 temp path");
        symlink("cellar-alias", &cellar).expect("Cellar symlink");

        let alias_bytes = std::fs::symlink_metadata(&alias)
            .expect("alias metadata")
            .len();
        assert_eq!(
            cellar_statistics(&cellar).expect("statistics"),
            (1, 0, alias_bytes)
        );
    }

    #[test]
    fn cellar_statistics_support_non_directory_root_target() {
        let temp = TempDir::new().expect("temp");
        let target =
            Utf8PathBuf::from_path_buf(temp.path().join("cellar-file")).expect("UTF-8 temp path");
        std::fs::write(&target, b"cellar").expect("target");
        let cellar =
            Utf8PathBuf::from_path_buf(temp.path().join("Cellar")).expect("UTF-8 temp path");
        symlink(&target, &cellar).expect("Cellar symlink");

        assert_eq!(cellar_statistics(&cellar).expect("statistics"), (0, 1, 6));
    }

    #[test]
    fn cellar_statistics_ignore_cyclic_version_symlink_for_rack_count() {
        let temp = TempDir::new().expect("temp");
        let cellar =
            Utf8PathBuf::from_path_buf(temp.path().join("Cellar")).expect("UTF-8 temp path");
        let rack = cellar.join("sample");
        std::fs::create_dir_all(&rack).expect("rack");
        let version_link = rack.join("1.0");
        symlink("1.0", &version_link).expect("cyclic version symlink");

        let expected_bytes = [&cellar, &rack]
            .into_iter()
            .map(|path| {
                std::fs::symlink_metadata(path)
                    .expect("directory metadata")
                    .len()
            })
            .sum::<u64>()
            + std::fs::symlink_metadata(&version_link)
                .expect("version link metadata")
                .len();

        assert_eq!(
            cellar_statistics(&cellar).expect("statistics"),
            (0, 1, expected_bytes)
        );
    }
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

fn render_cask(ctx: &Ctx, cask: &Cask, installed: Option<&InstalledCask>) -> Result<(), OpError> {
    let mut title = format!(
        "{}: {}",
        cask.token,
        cask.version.as_deref().unwrap_or("latest")
    );
    if cask.auto_updates {
        title.push_str(" (auto_updates)");
    }
    if !cask.name.is_empty() {
        title.push_str(&format!(" ({})", cask.name.join(", ")));
    }
    ctx.reporter.ohai(&title);

    if let Some(description) = &cask.desc {
        ctx.reporter.print(description);
    }
    if let Some(homepage) = &cask.homepage {
        ctx.reporter.print(homepage);
    }
    if !cask.old_tokens.is_empty() {
        ctx.reporter
            .print(&format!("Old Tokens: {}", cask.old_tokens.join(", ")));
    }

    match installed.and_then(InstalledCask::installed_version) {
        Some(version) => ctx.reporter.print(&format!("Installed ({version})")),
        None => ctx.reporter.print("Not installed"),
    }

    render_cask_dependencies(ctx, cask);
    render_cask_artifacts(ctx, cask)?;

    if !ctx.reporter.is_quiet()
        && let Some(caveats) = cask.caveats.as_deref().filter(|text| !text.is_empty())
    {
        ctx.reporter.ohai("Caveats");
        ctx.reporter.print(caveats);
    }
    Ok(())
}

fn render_cask_dependencies(ctx: &Ctx, cask: &Cask) {
    let mut lines = Vec::new();
    if !cask.depends_on.formula.is_empty() {
        lines.push(format!(
            "Formula ({}): {}",
            cask.depends_on.formula.len(),
            cask.depends_on.formula.join(", ")
        ));
    }
    if !cask.depends_on.cask.is_empty() {
        let names: Vec<String> = cask
            .depends_on
            .cask
            .iter()
            .map(|token| format!("{token} (cask)"))
            .collect();
        lines.push(format!("Cask ({}): {}", names.len(), names.join(", ")));
    }
    if lines.is_empty() {
        return;
    }
    ctx.reporter.ohai("Dependencies");
    for line in lines {
        ctx.reporter.print(&line);
    }
}

fn render_cask_artifacts(ctx: &Ctx, cask: &Cask) -> Result<(), OpError> {
    if cask.artifacts.is_empty() {
        return Ok(());
    }
    ctx.reporter.ohai("Artifacts");
    for artifact in &cask.artifacts {
        let value =
            serde_json::to_string(&artifact.value).map_err(|source| OpError::InvalidState {
                reason: format!("serialize cask artifact: {source}"),
            })?;
        ctx.reporter.print(&format!("{} {}", artifact.kind, value));
    }
    Ok(())
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
    casks: Vec<&'a Value>,
}

fn json_v2(formulae: &[&Formula], casks: &[&Cask]) -> Result<String, OpError> {
    let payload = JsonV2 {
        formulae: formulae.iter().map(|formula| &formula.raw).collect(),
        casks: casks.iter().map(|cask| &cask.raw).collect(),
    };
    serde_json::to_string_pretty(&payload).map_err(|source| OpError::InvalidState {
        reason: format!("serialize info JSON: {source}"),
    })
}
