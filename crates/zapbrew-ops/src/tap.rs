use std::ffi::OsStr;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use camino::{Utf8Path, Utf8PathBuf};
use zapbrew_prefix::Env;

use crate::platform::{git_clone, run_checked};
use crate::size::disk_usage_readable;
use crate::{Ctx, OpError};

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Args {
    pub name: Option<String>,
    pub url: Option<String>,
    pub force: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct TapName {
    name: String,
    user: String,
    repository: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct TapStats {
    pub(crate) files: u64,
    pub(crate) bytes: u64,
    pub(crate) formulae: u64,
    pub(crate) casks: u64,
    pub(crate) commands: u64,
}

impl TapName {
    pub(crate) fn parse(raw: &str) -> Result<Self, OpError> {
        let mut parts = raw.split('/');
        let user = parts.next().unwrap_or_default();
        let repository = parts.next().unwrap_or_default();
        if user.is_empty()
            || repository.is_empty()
            || parts.next().is_some()
            || matches!(user, "." | "..")
        {
            return Err(invalid_name(raw));
        }

        let user = user.to_ascii_lowercase();
        let repository = repository.to_ascii_lowercase();
        let repository = repository
            .strip_prefix("homebrew-")
            .unwrap_or(&repository)
            .to_owned();
        if repository.is_empty() || matches!(repository.as_str(), "." | "..") {
            return Err(invalid_name(raw));
        }

        let identity_user = match user.as_str() {
            "homebrew" => "Homebrew".to_owned(),
            "linuxbrew" => "Linuxbrew".to_owned(),
            _ => user.clone(),
        };
        Ok(Self {
            name: format!("{user}/{repository}"),
            user: identity_user,
            repository,
        })
    }

    pub(crate) fn name(&self) -> &str {
        &self.name
    }

    pub(crate) fn user(&self) -> &str {
        &self.user
    }

    pub(crate) fn repository(&self) -> &str {
        &self.repository
    }

    pub(crate) fn user_path(&self, env: &Env) -> Utf8PathBuf {
        taps_root(env).join(&self.user)
    }

    pub(crate) fn path(&self, env: &Env) -> Utf8PathBuf {
        self.user_path(env)
            .join(format!("homebrew-{}", self.repository))
    }

    fn default_remote(&self) -> String {
        format!(
            "https://github.com/{}/homebrew-{}",
            self.user, self.repository
        )
    }

    fn is_api_tap(&self) -> bool {
        matches!(self.user.as_str(), "Homebrew" | "Linuxbrew")
            && (self.repository == "core" || (self.user == "Homebrew" && self.repository == "cask"))
    }
}

impl TapStats {
    pub(crate) fn abv(self) -> String {
        format!("{} files, {}", self.files, disk_usage_readable(self.bytes))
    }
}

/// Canonical on-disk directory for a tap name such as `user/repo`.
///
/// Reuses [`TapName`] normalization — case-folding, `homebrew-` stripping, and
/// the Homebrew/Linuxbrew identity mapping — so the CLI `--repository <tap>`
/// fast path resolves the exact directory the tap operations use. Pure: no
/// network and no existence check.
pub fn repository_path(env: &Env, raw: &str) -> Result<Utf8PathBuf, OpError> {
    Ok(TapName::parse(raw)?.path(env))
}

pub async fn run(ctx: &Ctx, args: Args) -> Result<(), OpError> {
    let Some(raw_name) = args.name else {
        for tap in installed(&ctx.env)? {
            ctx.reporter.print(tap.name());
        }
        return Ok(());
    };

    let tap = TapName::parse(&raw_name)?;
    if tap.is_api_tap() && !args.force {
        return Err(OpError::Refusal {
            message: format!(
                "Tapping {} is no longer typically necessary.\nAdd --force if you are sure you need it for contributing to Homebrew.",
                tap.name()
            ),
        });
    }

    let destination = tap.path(&ctx.env);
    if path_exists(&destination)? {
        return Err(OpError::Refusal {
            message: format!("Tap {} already tapped.", tap.name()),
        });
    }
    prepare_parent(&ctx.env, &tap)?;

    let remote = args.url.unwrap_or_else(|| tap.default_remote());
    ctx.reporter.ohai(&format!("Tapping {}", tap.name()));
    if let Err(error) = run_checked(ctx.commands.as_ref(), &git_clone(&remote, &destination)) {
        if let Err(cleanup_error) = rollback_clone(&ctx.env, &tap, &destination) {
            ctx.reporter.opoo(&format!(
                "Failed to remove partial tap {}: {cleanup_error}",
                tap.name()
            ));
        }
        return Err(error);
    }
    let stats = measure(&destination)?;
    ctx.reporter.print(&format!("Tapped ({}).", stats.abv()));
    Ok(())
}

fn rollback_clone(env: &Env, tap: &TapName, destination: &Utf8Path) -> Result<(), OpError> {
    if path_exists(destination)? {
        remove_tree(destination)?;
    }
    let user_path = tap.user_path(env);
    if is_empty_real_directory(&user_path)? {
        fs::remove_dir(&user_path).map_err(|source| {
            OpError::io("remove empty tap user directory", user_path.clone(), source)
        })?;
    }
    Ok(())
}

fn invalid_name(raw: &str) -> OpError {
    OpError::Refusal {
        message: format!("Invalid tap name: '{raw}'"),
    }
}

pub(crate) fn taps_root(env: &Env) -> Utf8PathBuf {
    env.library.join("Taps")
}

pub(crate) fn installed(env: &Env) -> Result<Vec<TapName>, OpError> {
    let root = taps_root(env);
    if !is_real_directory(&root)? {
        return Ok(Vec::new());
    }

    let mut taps = Vec::new();
    for user_path in sorted_entries(&root)? {
        if !is_real_directory_path(&user_path)? {
            continue;
        }
        let Some(user) = user_path.file_name().and_then(OsStr::to_str) else {
            continue;
        };
        for tap_path in sorted_entries_path(&user_path)? {
            if !is_real_directory_path(&tap_path)? {
                continue;
            }
            let Some(directory) = tap_path.file_name().and_then(OsStr::to_str) else {
                continue;
            };
            let Some(repository) = directory.strip_prefix("homebrew-") else {
                continue;
            };
            let Ok(tap) = TapName::parse(&format!("{user}/{repository}")) else {
                continue;
            };
            if tap.path(env).as_std_path() == tap_path {
                taps.push(tap);
            }
        }
    }
    taps.sort();
    taps.dedup();
    Ok(taps)
}

pub(crate) fn is_installed(env: &Env, tap: &TapName) -> Result<bool, OpError> {
    if !is_real_directory(&taps_root(env))? || !is_real_directory(&tap.user_path(env))? {
        return Ok(false);
    }
    is_real_directory(&tap.path(env))
}

pub(crate) fn measure(path: &Utf8Path) -> Result<TapStats, OpError> {
    if !is_real_directory(path)? {
        return Err(OpError::Refusal {
            message: format!("Tap path is not a real directory: {path}"),
        });
    }

    let formula_root = if is_real_directory(&path.join("Formula"))? {
        Some("Formula")
    } else if is_real_directory(&path.join("HomebrewFormula"))? {
        Some("HomebrewFormula")
    } else {
        None
    };
    let mut stats = TapStats::default();
    visit(
        path.as_std_path(),
        path.as_std_path(),
        formula_root,
        &mut stats,
    )?;
    Ok(stats)
}

fn visit(
    root: &Path,
    directory: &Path,
    formula_root: Option<&str>,
    stats: &mut TapStats,
) -> Result<(), OpError> {
    for path in sorted_entries_path(directory)? {
        let metadata = fs::symlink_metadata(&path)
            .map_err(|source| io_error("inspect tap entry", &path, source))?;
        if metadata.is_dir() {
            visit(root, &path, formula_root, stats)?;
            continue;
        }

        stats.files += 1;
        stats.bytes = stats.bytes.saturating_add(metadata.len());
        if !metadata.is_file() {
            continue;
        }
        let relative = path.strip_prefix(root).map_err(|_| OpError::InvalidState {
            reason: format!("tap entry escaped its root: {}", path.display()),
        })?;
        let first = relative
            .components()
            .next()
            .and_then(|part| part.as_os_str().to_str());
        let extension = path.extension().and_then(OsStr::to_str);
        let file_name = path.file_name().and_then(OsStr::to_str).unwrap_or_default();
        let component_count = relative.components().count();

        if extension == Some("rb")
            && formula_root.map_or(component_count == 1, |root| first == Some(root))
        {
            stats.formulae += 1;
        } else if extension == Some("rb") && first == Some("Casks") {
            stats.casks += 1;
        } else if first == Some("cmd") && file_name.starts_with("brew-") {
            stats.commands += 1;
        }
    }
    Ok(())
}

pub(crate) fn remove_tree(path: &Utf8Path) -> Result<(), OpError> {
    fs::remove_dir_all(path)
        .map_err(|source| OpError::io("remove tap directory", path.to_path_buf(), source))
}

pub(crate) fn is_empty_real_directory(path: &Utf8Path) -> Result<bool, OpError> {
    if !is_real_directory(path)? {
        return Ok(false);
    }
    Ok(sorted_entries(path)?.is_empty())
}

fn prepare_parent(env: &Env, tap: &TapName) -> Result<(), OpError> {
    fs::create_dir_all(&env.library)
        .map_err(|source| OpError::io("create tap library", env.library.clone(), source))?;
    if !is_real_directory(&env.library)? {
        return Err(OpError::Refusal {
            message: format!("Tap library is not a real directory: {}", env.library),
        });
    }

    let root = taps_root(env);
    create_real_directory(&root, "create tap root")?;
    create_real_directory(&tap.user_path(env), "create tap user directory")
}

fn create_real_directory(path: &Utf8Path, operation: &'static str) -> Result<(), OpError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => Ok(()),
        Ok(_) => Err(OpError::Refusal {
            message: format!("Tap path is not a real directory: {path}"),
        }),
        Err(source) if source.kind() == io::ErrorKind::NotFound => fs::create_dir(path)
            .map_err(|source| OpError::io(operation, path.to_path_buf(), source)),
        Err(source) => Err(OpError::io(operation, path.to_path_buf(), source)),
    }
}

fn path_exists(path: &Utf8Path) -> Result<bool, OpError> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(source) => Err(OpError::io("inspect tap path", path.to_path_buf(), source)),
    }
}

fn is_real_directory(path: &Utf8Path) -> Result<bool, OpError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => Ok(metadata.is_dir() && !metadata.file_type().is_symlink()),
        Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(source) => Err(OpError::io(
            "inspect tap directory",
            path.to_path_buf(),
            source,
        )),
    }
}

fn is_real_directory_path(path: &Path) -> Result<bool, OpError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => Ok(metadata.is_dir() && !metadata.file_type().is_symlink()),
        Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(source) => Err(io_error("inspect tap directory", path, source)),
    }
}

fn sorted_entries(path: &Utf8Path) -> Result<Vec<PathBuf>, OpError> {
    sorted_entries_path(path.as_std_path())
}

fn sorted_entries_path(path: &Path) -> Result<Vec<PathBuf>, OpError> {
    let entries =
        fs::read_dir(path).map_err(|source| io_error("read tap directory", path, source))?;
    let mut paths = entries
        .map(|entry| {
            entry
                .map(|entry| entry.path())
                .map_err(|source| io_error("read tap entry", path, source))
        })
        .collect::<Result<Vec<_>, _>>()?;
    paths.sort();
    Ok(paths)
}

fn io_error(operation: &'static str, path: &Path, source: io::Error) -> OpError {
    OpError::io(
        operation,
        Utf8PathBuf::from(path.to_string_lossy().into_owned()),
        source,
    )
}

#[cfg(test)]
mod repository_path_tests {
    use std::collections::HashMap;

    use zapbrew_prefix::{Env, EnvDetectInput, SystemCommandRunner};

    use super::repository_path;

    fn scratch_env() -> Env {
        Env::detect_from(
            &EnvDetectInput {
                os: "linux".to_owned(),
                arch: "x86_64".to_owned(),
                home: "/home/test".into(),
                xdg_cache_home: None,
                vars: HashMap::from([("HOMEBREW_PREFIX".to_owned(), "/opt/zapbrew".to_owned())]),
                available_parallelism: 2,
            },
            &SystemCommandRunner,
        )
        .expect("scratch env")
    }

    #[test]
    fn canonical_tap_resolves_under_library_taps() {
        let env = scratch_env();
        let path = repository_path(&env, "homebrew/core").expect("path");
        assert_eq!(path, env.library.join("Taps/Homebrew/homebrew-core"));
    }

    #[test]
    fn strips_homebrew_prefix_and_lowercases_user_and_repo() {
        let env = scratch_env();
        let path = repository_path(&env, "User/homebrew-Fun").expect("path");
        assert_eq!(path, env.library.join("Taps/user/homebrew-fun"));
    }

    #[test]
    fn invalid_tap_name_is_refused() {
        let env = scratch_env();
        assert!(repository_path(&env, "no-slash").is_err());
    }
}
