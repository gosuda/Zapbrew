//! Shell completion generation and link management for the `completions` subcommand.
//!
//! `main` intercepts the whole `completions` surface before catalog or network
//! work. `generate` is a pure function of the clap surface and is emitted to
//! stdout. `state`, `link`, and `unlink` are confined to the active prefix: they
//! only touch the managed source directory (`<repository>/completions`) and the
//! standard shell completion directories, never unrelated files.

use std::collections::BTreeMap;
use std::fs;
use std::io::{self, Write};

use camino::{Utf8Component, Utf8Path, Utf8PathBuf};
use clap::CommandFactory;
use clap_complete::Shell as ClapShell;
use zapbrew_ops::{OpError, Reporter};
use zapbrew_prefix::{Env, LockGuard};

use crate::cli::{Cli, CompletionShell, CompletionsArgs, CompletionsCommand};

/// Build an [`OpError::Io`] because the external `OpError::io` constructor is
/// crate-private.
fn io_err(operation: &'static str, path: &Utf8Path, source: std::io::Error) -> OpError {
    OpError::Io {
        operation,
        path: path.to_owned(),
        source,
    }
}

/// The binary name every generated script drives.
const BIN_NAME: &str = "zapbrew";

/// Bash completion source shipped with the binary.
const BASH_SOURCE: &str = include_str!("../assets/completions/bash/zapbrew");
/// Zsh completion source shipped with the binary.
const ZSH_SOURCE: &str = include_str!("../assets/completions/zsh/_zapbrew");
/// Fish completion source shipped with the binary.
const FISH_SOURCE: &str = include_str!("../assets/completions/fish/zapbrew.fish");

/// One shell worth of managed completion files.
#[derive(Debug, Clone, Copy)]
struct ShellConfig {
    /// Source filename inside `<repository>/completions/<subdir>`.
    source_name: &'static str,
    /// Subdirectory inside the managed source dir.
    source_subdir: &'static str,
    /// Destination directory under the prefix.
    destination_subdir: &'static str,
    /// Embedded content to install when the source is missing.
    source_content: &'static str,
}

const SHELLS: &[ShellConfig] = &[
    ShellConfig {
        source_name: "zapbrew",
        source_subdir: "bash",
        destination_subdir: "etc/bash_completion.d",
        source_content: BASH_SOURCE,
    },
    ShellConfig {
        source_name: "_zapbrew",
        source_subdir: "zsh",
        destination_subdir: "share/zsh/site-functions",
        source_content: ZSH_SOURCE,
    },
    ShellConfig {
        source_name: "zapbrew.fish",
        source_subdir: "fish",
        destination_subdir: "share/fish/vendor_completions.d",
        source_content: FISH_SOURCE,
    },
];

/// Run the requested completion operation after environment detection.
///
/// `generate` is normally handled by `main` before any environment setup; this
/// dispatcher treats it as a defensive no-op that re-emits the script.
pub fn run(args: &CompletionsArgs, env: &Env, reporter: &dyn Reporter) -> Result<(), OpError> {
    if args.shell.is_some() {
        return Err(OpError::InvalidState {
            reason: "completions <shell> must be handled by main before environment setup"
                .to_owned(),
        });
    }
    match &args.command {
        None | Some(CompletionsCommand::State) => state(env, reporter),
        Some(CompletionsCommand::Link) => link(env, reporter),
        Some(CompletionsCommand::Unlink) => unlink(env, reporter),
        Some(CompletionsCommand::Generate { .. }) => Err(OpError::InvalidState {
            reason: "completions generate must be handled by main before environment setup"
                .to_owned(),
        }),
    }
}

/// Write the completion script for `shell` to `out`.
///
/// The generator only produces stdout content; it never emits ANSI styling or
/// diagnostics, so `out` receives a ready-to-source script.
pub fn generate(shell: CompletionShell, out: &mut dyn Write) {
    let generator = match shell {
        CompletionShell::Bash => ClapShell::Bash,
        CompletionShell::Zsh => ClapShell::Zsh,
        CompletionShell::Fish => ClapShell::Fish,
    };
    let mut command = Cli::command();
    clap_complete::generate(generator, &mut command, BIN_NAME, out);
}

/// Report the current completion link state.
///
/// Homebrew prints one deterministic line; we mirror that by treating all three
/// managed links as a single "linked" state.
fn state(env: &Env, reporter: &dyn Reporter) -> Result<(), OpError> {
    if all_linked(env)? {
        reporter.print("Completions are linked.");
    } else {
        reporter.print("Completions are not linked.");
    }
    Ok(())
}

/// One source file and its managed destination.
#[derive(Debug, Clone)]
struct ManagedFile {
    source: Utf8PathBuf,
    destination: Utf8PathBuf,
    static_content: Option<&'static str>,
}

/// Idempotently install shipped assets and link every eligible completion file.
///
/// All source and destination conflicts are rejected before the first write,
/// directory creation, symlink replacement, or other filesystem mutation.
fn link(env: &Env, reporter: &dyn Reporter) -> Result<(), OpError> {
    let _lock = LockGuard::acquire(&env.locks, "completions.lock")?;
    let files = managed_files(env)?;
    preflight_link(env, &files)?;
    install_sources(&files)?;

    for file in &files {
        link_one(&file.source, &file.destination)?;
    }
    reporter.print("Completions are now linked.");
    Ok(())
}

/// Idempotently remove only managed completion links; source assets and
/// unrelated files are never touched.
fn unlink(env: &Env, reporter: &dyn Reporter) -> Result<(), OpError> {
    let _lock = LockGuard::acquire(&env.locks, "completions.lock")?;
    let files = managed_files(env)?;
    preflight_destinations(env, &files)?;
    for file in &files {
        unlink_one(&file.source, &file.destination)?;
    }
    reporter.print("Completions are no longer linked.");
    Ok(())
}

/// Materialize only the three embedded Zapbrew sources. Tap sources already
/// exist on disk and are never rewritten.
fn install_sources(files: &[ManagedFile]) -> Result<(), OpError> {
    for file in files {
        let Some(content) = file.static_content else {
            continue;
        };
        create_parent(&file.source)?;
        fs::write(file.source.as_std_path(), content)
            .map_err(|source| io_err("write", &file.source, source))?;
    }
    Ok(())
}

/// Discover the three shipped sources and completion files from installed taps.
/// Tap destination collisions follow deterministic last-source-wins ordering:
/// taps are ordered by user/repository and files lexicographically. Zapbrew's
/// own three filenames remain authoritative over colliding tap files.
fn managed_files(env: &Env) -> Result<Vec<ManagedFile>, OpError> {
    let mut by_destination = BTreeMap::<Utf8PathBuf, ManagedFile>::new();
    let taps = env.repository.join("Library/Taps");
    for user in sorted_real_directories(&taps)? {
        for repository in sorted_real_directories(&user)? {
            for shell in SHELLS {
                let root = repository.join("completions").join(shell.source_subdir);
                let mut discovered = Vec::new();
                collect_completion_files(&root, &root, &mut discovered)?;
                discovered.sort();
                for source in discovered {
                    let relative =
                        source
                            .strip_prefix(&root)
                            .map_err(|_| OpError::InvalidState {
                                reason: format!("completion source escaped {root}: {source}"),
                            })?;
                    let file = ManagedFile {
                        destination: destination_path(env, shell, relative),
                        source,
                        static_content: None,
                    };
                    by_destination.insert(file.destination.clone(), file);
                }
            }
        }
    }
    // Zapbrew's own assets win a filename collision with a tap, just as the
    // repository's built-in completion remains authoritative for its command.
    for shell in SHELLS {
        let file = ManagedFile {
            source: source_path(env, shell),
            destination: destination_path(env, shell, Utf8Path::new(shell.source_name)),
            static_content: Some(shell.source_content),
        };
        by_destination.insert(file.destination.clone(), file);
    }
    Ok(by_destination.into_values().collect())
}

/// Return real child directories in lexical order. Symlink directories are not
/// traversed, preventing an installed tap from redirecting discovery elsewhere.
fn sorted_real_directories(root: &Utf8Path) -> Result<Vec<Utf8PathBuf>, OpError> {
    let entries = match fs::read_dir(root.as_std_path()) {
        Ok(entries) => entries,
        Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(source) => return Err(io_err("read", root, source)),
    };
    let mut directories = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| io_err("read", root, source))?;
        let path = Utf8PathBuf::from_path_buf(entry.path()).map_err(|path| OpError::Refusal {
            message: format!("completion path is not valid UTF-8: {}", path.display()),
        })?;
        let metadata = fs::symlink_metadata(path.as_std_path())
            .map_err(|source| io_err("inspect", &path, source))?;
        if metadata.is_dir() && !metadata.file_type().is_symlink() {
            directories.push(path);
        }
    }
    directories.sort();
    Ok(directories)
}

/// Walk a completion source tree without following directory symlinks.
fn collect_completion_files(
    root: &Utf8Path,
    current: &Utf8Path,
    files: &mut Vec<Utf8PathBuf>,
) -> Result<(), OpError> {
    let entries = match fs::read_dir(current.as_std_path()) {
        Ok(entries) => entries,
        Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(source) => return Err(io_err("read", current, source)),
    };
    let mut paths = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|source| io_err("read", current, source))?;
        let path = Utf8PathBuf::from_path_buf(entry.path()).map_err(|path| OpError::Refusal {
            message: format!("completion path is not valid UTF-8: {}", path.display()),
        })?;
        paths.push(path);
    }
    paths.sort();
    for path in paths {
        if !path.starts_with(root) {
            return Err(OpError::InvalidState {
                reason: format!("completion source escaped {root}: {path}"),
            });
        }
        let metadata = fs::symlink_metadata(path.as_std_path())
            .map_err(|source| io_err("inspect", &path, source))?;
        if metadata.is_dir() && !metadata.file_type().is_symlink() {
            collect_completion_files(root, &path, files)?;
        } else if metadata.is_file() || metadata.file_type().is_symlink() {
            files.push(path);
        }
    }
    Ok(())
}

/// Reject every source/destination conflict and unsafe ancestor before mutation.
fn preflight_link(env: &Env, files: &[ManagedFile]) -> Result<(), OpError> {
    preflight_destinations(env, files)?;
    let mut conflicts = Vec::new();
    for file in files {
        if file.static_content.is_none() {
            continue;
        }
        let source_root = if file.source.starts_with(&env.prefix) {
            &env.prefix
        } else {
            ensure_real_root(&env.repository)?;
            &env.repository
        };
        ensure_no_symlink_ancestors(source_root, &file.source)?;
        match fs::symlink_metadata(file.source.as_std_path()) {
            Err(source) if source.kind() == io::ErrorKind::NotFound => {}
            Err(source) => return Err(io_err("inspect", &file.source, source)),
            Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {}
            Ok(_) => conflicts.push(file.source.to_string()),
        }
    }
    for file in files {
        match fs::symlink_metadata(file.destination.as_std_path()) {
            Err(source) if source.kind() == io::ErrorKind::NotFound => {}
            Err(source) => return Err(io_err("inspect", &file.destination, source)),
            Ok(metadata) if metadata.file_type().is_symlink() => {}
            Ok(_) => conflicts.push(file.destination.to_string()),
        }
    }
    conflicts.sort();
    conflicts.dedup();
    if conflicts.is_empty() {
        Ok(())
    } else {
        Err(OpError::Refusal {
            message: format!(
                "Could not link:\n{}\n\nPlease delete these paths and run:\n  zapbrew completions link",
                conflicts.join("\n")
            ),
        })
    }
}

/// Verify existing destination ancestors are real directories under the prefix.
fn preflight_destinations(env: &Env, files: &[ManagedFile]) -> Result<(), OpError> {
    ensure_real_root(&env.prefix)?;
    for file in files {
        if !file.destination.starts_with(&env.prefix) {
            return Err(OpError::InvalidState {
                reason: format!(
                    "completion destination escaped {}: {}",
                    env.prefix, file.destination
                ),
            });
        }
        ensure_no_symlink_ancestors(&env.prefix, &file.destination)?;
    }
    Ok(())
}

fn ensure_real_root(root: &Utf8Path) -> Result<(), OpError> {
    let metadata = fs::symlink_metadata(root.as_std_path())
        .map_err(|source| io_err("inspect", root, source))?;
    if metadata.is_dir() && !metadata.file_type().is_symlink() {
        Ok(())
    } else {
        Err(OpError::Refusal {
            message: format!("completion prefix is not a real directory: {root}"),
        })
    }
}

fn ensure_no_symlink_ancestors(root: &Utf8Path, path: &Utf8Path) -> Result<(), OpError> {
    let relative = path.strip_prefix(root).map_err(|_| OpError::InvalidState {
        reason: format!("completion path escaped {root}: {path}"),
    })?;
    let mut current = root.to_path_buf();
    for component in relative.components() {
        current.push(component.as_str());
        if current == path {
            break;
        }
        match fs::symlink_metadata(current.as_std_path()) {
            Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(source) => return Err(io_err("inspect", &current, source)),
            Ok(metadata) if metadata.is_dir() && !metadata.file_type().is_symlink() => {}
            Ok(_) => {
                return Err(OpError::Refusal {
                    message: format!("completion path has a non-directory ancestor: {current}"),
                });
            }
        }
    }
    Ok(())
}

fn link_one(source: &Utf8Path, destination: &Utf8Path) -> Result<(), OpError> {
    match fs::symlink_metadata(destination.as_std_path()) {
        Err(err) if err.kind() == io::ErrorKind::NotFound => create_symlink(source, destination),
        Err(err) => Err(io_err("inspect", destination, err)),
        Ok(metadata) if metadata.file_type().is_symlink() => {
            if matches_managed_target(destination, source)? {
                return Ok(());
            }
            fs::remove_file(destination.as_std_path())
                .map_err(|err| io_err("remove", destination, err))?;
            create_symlink(source, destination)
        }
        Ok(_) => Err(OpError::InvalidState {
            reason: format!("completion conflict escaped preflight: {destination}"),
        }),
    }
}

fn unlink_one(source: &Utf8Path, destination: &Utf8Path) -> Result<(), OpError> {
    match fs::symlink_metadata(destination.as_std_path()) {
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(err) => Err(io_err("inspect", destination, err)),
        Ok(metadata) if !metadata.file_type().is_symlink() => Ok(()),
        Ok(_) => {
            if matches_managed_target(destination, source)? {
                fs::remove_file(destination.as_std_path())
                    .map_err(|err| io_err("remove", destination, err))?;
                remove_empty_parent(destination);
            }
            Ok(())
        }
    }
}

fn all_linked(env: &Env) -> Result<bool, OpError> {
    let files = managed_files(env)?;
    for file in &files {
        match fs::symlink_metadata(file.destination.as_std_path()) {
            Err(source) if source.kind() == io::ErrorKind::NotFound => return Ok(false),
            Err(source) => return Err(io_err("inspect", &file.destination, source)),
            Ok(metadata) if !metadata.file_type().is_symlink() => return Ok(false),
            Ok(_) if !matches_managed_target(&file.destination, &file.source)? => return Ok(false),
            Ok(_) => {}
        }
    }
    Ok(true)
}

/// True when `link` resolves to `target`.
///
/// The comparison is on lexically-normalized absolute paths so relative
/// symlinks stay portable and a stale or dangling link is still detected if its
/// target path matches the managed source.
fn matches_managed_target(link: &Utf8Path, target: &Utf8Path) -> Result<bool, OpError> {
    let parent = link.parent().ok_or_else(|| OpError::InvalidState {
        reason: format!("completion link has no parent: {link}"),
    })?;
    let raw = fs::read_link(link.as_std_path()).map_err(|source| io_err("read", link, source))?;
    let raw = Utf8PathBuf::from_path_buf(raw).map_err(|raw| OpError::Refusal {
        message: format!(
            "completion link target is not valid UTF-8: {}",
            raw.display()
        ),
    })?;
    let joined = if raw.is_absolute() {
        raw
    } else {
        parent.join(raw)
    };
    let Some(resolved) = normalize(joined.as_std_path()) else {
        return Ok(false);
    };
    let resolved = Utf8PathBuf::from_path_buf(resolved).map_err(|resolved| OpError::Refusal {
        message: format!(
            "normalized link target is not valid UTF-8: {}",
            resolved.display()
        ),
    })?;
    let Some(normalized_target) = normalize(target.as_std_path()) else {
        return Ok(false);
    };
    let normalized_target =
        Utf8PathBuf::from_path_buf(normalized_target).map_err(|target| OpError::Refusal {
            message: format!(
                "normalized source target is not valid UTF-8: {}",
                target.display()
            ),
        })?;
    Ok(resolved == normalized_target)
}

#[cfg(unix)]
fn create_symlink(source: &Utf8Path, destination: &Utf8Path) -> Result<(), OpError> {
    use std::os::unix::fs::symlink as unix_symlink;

    create_parent(destination)?;
    let target = relative_path_from(source, destination).ok_or_else(|| OpError::Refusal {
        message: format!("cannot compute relative path from {destination} to {source}"),
    })?;
    unix_symlink(target.as_std_path(), destination.as_std_path())
        .map_err(|source| io_err("symlink", destination, source))
}

#[cfg(not(unix))]
fn create_symlink(source: &Utf8Path, destination: &Utf8Path) -> Result<(), OpError> {
    create_parent(destination)?;
    fs::copy(source.as_std_path(), destination.as_std_path())
        .map(|_| ())
        .map_err(|source| io_err("copy", destination, source))
}

fn create_parent(path: &Utf8Path) -> Result<(), OpError> {
    let Some(parent) = path.parent() else {
        return Err(OpError::InvalidState {
            reason: format!("completion path has no parent: {path}"),
        });
    };
    fs::create_dir_all(parent.as_std_path()).map_err(|source| io_err("create", parent, source))
}

fn remove_empty_parent(path: &Utf8Path) {
    if let Some(parent) = path.parent() {
        let _ = fs::remove_dir(parent.as_std_path());
    }
}

fn source_root(env: &Env) -> Utf8PathBuf {
    env.repository.join("completions")
}

fn source_dir(env: &Env, shell: &ShellConfig) -> Utf8PathBuf {
    source_root(env).join(shell.source_subdir)
}

fn source_path(env: &Env, shell: &ShellConfig) -> Utf8PathBuf {
    source_dir(env, shell).join(shell.source_name)
}

fn destination_path(env: &Env, shell: &ShellConfig, relative: &Utf8Path) -> Utf8PathBuf {
    env.prefix.join(shell.destination_subdir).join(relative)
}

/// Compute a relative path from the parent of `link` to `target`, both absolute
/// POSIX paths. Returns `None` when the paths share no common ancestor.
///
/// This mirrors `zapbrew-pour::relocate::relative_path_from` but stays local to
/// the CLI crate.
fn relative_path_from(target: &Utf8Path, link: &Utf8Path) -> Option<Utf8PathBuf> {
    let from = link.parent()?;
    let from_components: Vec<Utf8Component> = from.components().collect();
    let to_components: Vec<Utf8Component> = target.components().collect();

    let mut common = 0;
    while common < from_components.len()
        && common < to_components.len()
        && from_components[common] == to_components[common]
    {
        common += 1;
    }

    let mut result = Utf8PathBuf::new();
    for _ in common..from_components.len() {
        result.push("..");
    }
    for component in &to_components[common..] {
        match component {
            Utf8Component::CurDir | Utf8Component::RootDir => {}
            Utf8Component::ParentDir => result.push(".."),
            Utf8Component::Normal(name) => result.push(name),
            Utf8Component::Prefix(_) => return None,
        }
    }

    if result.as_str().is_empty() {
        result.push(".");
    }
    Some(result)
}

/// Lexically normalize a path, removing `.` and `..` without touching the
/// filesystem. Returns `None` for a path that escapes its root.
fn normalize(path: &std::path::Path) -> Option<std::path::PathBuf> {
    let mut normalized = std::path::PathBuf::new();
    for component in path.components() {
        match component {
            std::path::Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            std::path::Component::RootDir => normalized.push(std::path::Path::new("/")),
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                if !normalized.pop() {
                    return None;
                }
            }
            std::path::Component::Normal(segment) => normalized.push(segment),
        }
    }
    Some(normalized)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_emits_zapbrew_registration_for_every_shell() {
        for (shell, marker) in [
            (CompletionShell::Bash, "_zapbrew"),
            (CompletionShell::Zsh, "#compdef zapbrew"),
            (CompletionShell::Fish, "complete -c zapbrew"),
        ] {
            let mut buffer = Vec::new();
            generate(shell, &mut buffer);
            let text = String::from_utf8(buffer).expect("clap_complete emits UTF-8");
            assert!(text.contains(marker), "{shell:?} missing {marker}");
            assert!(text.contains(BIN_NAME), "{shell:?} missing {BIN_NAME}");
            assert!(!text.contains('\u{1b}'), "{shell:?} contains ANSI");
        }
    }

    #[test]
    fn static_assets_name_zapbrew_for_all_shells() {
        assert!(BASH_SOURCE.contains(BIN_NAME));
        assert!(ZSH_SOURCE.contains(BIN_NAME));
        assert!(FISH_SOURCE.contains(BIN_NAME));
    }

    #[test]
    fn embedded_assets_match_fresh_generation() {
        for (shell, source) in [
            (CompletionShell::Bash, BASH_SOURCE),
            (CompletionShell::Zsh, ZSH_SOURCE),
            (CompletionShell::Fish, FISH_SOURCE),
        ] {
            let mut buffer = Vec::new();
            generate(shell, &mut buffer);
            assert_eq!(
                buffer,
                source.as_bytes(),
                "{shell:?} embedded completion asset does not match fresh generation"
            );
        }
    }

    #[test]
    fn relative_path_from_prefix_and_cellar() {
        let target = Utf8Path::new("/opt/homebrew/completions/bash/zapbrew");
        let link = Utf8Path::new("/opt/homebrew/etc/bash_completion.d/zapbrew");
        let rel = relative_path_from(target, link).expect("relative");
        assert_eq!(rel.as_str(), "../../completions/bash/zapbrew");
    }

    #[test]
    fn relative_path_from_repository_under_prefix() {
        let target = Utf8Path::new("/home/linuxbrew/.linuxbrew/Homebrew/completions/bash/zapbrew");
        let link = Utf8Path::new("/home/linuxbrew/.linuxbrew/etc/bash_completion.d/zapbrew");
        let rel = relative_path_from(target, link).expect("relative");
        assert_eq!(rel.as_str(), "../../Homebrew/completions/bash/zapbrew");
    }

    #[test]
    fn normalize_removes_dotdot() {
        let path = std::path::Path::new("/a/b/c/../../d");
        let out = normalize(path).expect("normalizable");
        assert_eq!(out, std::path::Path::new("/a/d"));
    }
}
