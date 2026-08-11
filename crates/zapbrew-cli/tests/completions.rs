//! End-to-end behavior for the `completions` subcommand.
//!
//! `completions` is intercepted before catalog or network work, so these tests
//! run the real `zapbrew` binary in an isolated prefix and exercise state,
//! idempotent link, idempotent unlink, and confinement invariants.

use assert_cmd::Command;
use camino::Utf8Path;
use tempfile::TempDir;
use zapbrew_prefix::LockGuard;

/// Run `zapbrew` in an isolated prefix with the given arguments.
fn run(args: &[&str]) -> Command {
    let tmp = TempDir::new().expect("temp dir");
    let mut cmd = Command::cargo_bin("zapbrew").expect("binary");
    cmd.env("HOMEBREW_PREFIX", tmp.path())
        .env("HOMEBREW_REPOSITORY", tmp.path())
        // A bad API domain proves no catalog/network initialization runs.
        .env("HOMEBREW_API_DOMAIN", "http://localhost:1");
    for arg in args {
        cmd.arg(arg);
    }
    cmd
}

#[test]
fn generate_bash_completions_smoke() {
    let output = run(&["completions", "generate", "bash"])
        .output()
        .expect("run completions");
    assert!(output.status.success(), "bash completions exit status");
    assert!(output.stderr.is_empty(), "bash completions wrote to stderr");
    let stdout = String::from_utf8(output.stdout).expect("stdout is UTF-8");
    assert!(stdout.contains("_zapbrew"), "bash missing _zapbrew");
    assert!(stdout.contains("zapbrew"), "bash missing zapbrew");
}

#[test]
fn generate_zsh_completions_smoke() {
    let output = run(&["completions", "generate", "zsh"])
        .output()
        .expect("run completions");
    assert!(output.status.success(), "zsh completions exit status");
    assert!(output.stderr.is_empty(), "zsh completions wrote to stderr");
    let stdout = String::from_utf8(output.stdout).expect("stdout is UTF-8");
    assert!(stdout.contains("#compdef zapbrew"), "zsh missing #compdef");
    assert!(stdout.contains("zapbrew"), "zsh missing zapbrew");
}

#[test]
fn generate_fish_completions_smoke() {
    let output = run(&["completions", "generate", "fish"])
        .output()
        .expect("run completions");
    assert!(output.status.success(), "fish completions exit status");
    assert!(output.stderr.is_empty(), "fish completions wrote to stderr");
    let stdout = String::from_utf8(output.stdout).expect("stdout is UTF-8");
    assert!(
        stdout.contains("complete -c zapbrew"),
        "fish missing complete"
    );
    assert!(stdout.contains("zapbrew"), "fish missing zapbrew");
}

#[test]
fn bare_and_state_report_not_linked() {
    for args in [&["completions"][..], &["completions", "state"][..]] {
        let output = run(args).output().expect("run state");
        assert!(output.status.success(), "state exit status");
        let stdout = String::from_utf8(output.stdout).expect("stdout is UTF-8");
        assert!(
            stdout.contains("Completions are not linked."),
            "state should report not linked: {args:?}"
        );
    }
}

#[test]
fn link_creates_managed_sources_and_symlinks() {
    let tmp = TempDir::new().expect("temp dir");
    let mut link = Command::cargo_bin("zapbrew").expect("binary");
    link.env("HOMEBREW_PREFIX", tmp.path())
        .env("HOMEBREW_REPOSITORY", tmp.path())
        .env("HOMEBREW_API_DOMAIN", "http://localhost:1")
        .args(["completions", "link"]);
    let output = link.output().expect("run link");
    assert!(output.status.success(), "link exit status");
    let stdout = String::from_utf8(output.stdout).expect("stdout is UTF-8");
    assert!(stdout.contains("Completions are now linked."));

    // Source assets are installed under the repository.
    assert!(tmp.path().join("completions/bash/zapbrew").is_file());
    assert!(tmp.path().join("completions/zsh/_zapbrew").is_file());
    assert!(tmp.path().join("completions/fish/zapbrew.fish").is_file());

    // Managed links are created in the shell completion dirs.
    let bash = tmp.path().join("etc/bash_completion.d/zapbrew");
    let zsh = tmp.path().join("share/zsh/site-functions/_zapbrew");
    let fish = tmp
        .path()
        .join("share/fish/vendor_completions.d/zapbrew.fish");
    assert!(bash.is_symlink(), "bash link");
    assert!(zsh.is_symlink(), "zsh link");
    assert!(fish.is_symlink(), "fish link");

    // Symlinks resolve to the managed source.
    for (link, source) in [
        (&bash, "completions/bash/zapbrew"),
        (&zsh, "completions/zsh/_zapbrew"),
        (&fish, "completions/fish/zapbrew.fish"),
    ] {
        let target = std::fs::read_link(link).expect("read link");
        let parent = link.parent().expect("link parent");
        let resolved = parent.join(target);
        let resolved = resolved.canonicalize().expect("canonicalize");
        assert_eq!(
            resolved,
            tmp.path()
                .join(source)
                .canonicalize()
                .expect("canonicalize")
        );
    }
}

#[test]
fn link_is_idempotent() {
    let tmp = TempDir::new().expect("temp dir");
    let mut cmd = Command::cargo_bin("zapbrew").expect("binary");
    cmd.env("HOMEBREW_PREFIX", tmp.path())
        .env("HOMEBREW_REPOSITORY", tmp.path())
        .env("HOMEBREW_API_DOMAIN", "http://localhost:1")
        .args(["completions", "link"]);
    let first = cmd.output().expect("run first link");
    assert!(first.status.success());

    // Run a second time; it must succeed without creating new paths.
    let second = cmd.output().expect("run second link");
    assert!(second.status.success());
}

#[test]
fn state_after_link_reports_linked() {
    let tmp = TempDir::new().expect("temp dir");
    let mut cmd = Command::cargo_bin("zapbrew").expect("binary");
    cmd.env("HOMEBREW_PREFIX", tmp.path())
        .env("HOMEBREW_REPOSITORY", tmp.path())
        .env("HOMEBREW_API_DOMAIN", "http://localhost:1");

    cmd.args(["completions", "link"]);
    let output = cmd.output().expect("run link");
    assert!(output.status.success());

    let mut cmd = Command::cargo_bin("zapbrew").expect("binary");
    cmd.env("HOMEBREW_PREFIX", tmp.path())
        .env("HOMEBREW_REPOSITORY", tmp.path())
        .env("HOMEBREW_API_DOMAIN", "http://localhost:1")
        .args(["completions", "state"]);
    let output = cmd.output().expect("run state");
    assert!(output.status.success());
    let stdout = String::from_utf8(output.stdout).expect("stdout is UTF-8");
    assert!(stdout.contains("Completions are linked."));
}

#[test]
fn unlink_removes_only_managed_links() {
    let tmp = TempDir::new().expect("temp dir");
    let mut cmd = Command::cargo_bin("zapbrew").expect("binary");
    cmd.env("HOMEBREW_PREFIX", tmp.path())
        .env("HOMEBREW_REPOSITORY", tmp.path())
        .env("HOMEBREW_API_DOMAIN", "http://localhost:1");

    // Set up a managed link plus an unrelated file in the same dir.
    cmd.args(["completions", "link"]);
    let output = cmd.output().expect("run link");
    assert!(output.status.success());

    let fish_dir = tmp.path().join("share/fish/vendor_completions.d");
    let unrelated = fish_dir.join("other.fish");
    std::fs::write(&unrelated, "foreign").expect("write unrelated");

    // Run unlink twice; it must be idempotent and leave the unrelated file.
    for _ in 0..2 {
        let mut cmd = Command::cargo_bin("zapbrew").expect("binary");
        cmd.env("HOMEBREW_PREFIX", tmp.path())
            .env("HOMEBREW_REPOSITORY", tmp.path())
            .env("HOMEBREW_API_DOMAIN", "http://localhost:1")
            .args(["completions", "unlink"]);
        let output = cmd.output().expect("run unlink");
        assert!(output.status.success(), "unlink exit status");
        let stdout = String::from_utf8(output.stdout).expect("stdout is UTF-8");
        assert!(stdout.contains("Completions are no longer linked."));
    }

    assert!(unrelated.exists(), "unrelated file must survive unlink");
    assert!(
        !fish_dir.join("zapbrew.fish").exists(),
        "managed link must be removed"
    );
    assert!(
        tmp.path().join("completions/fish/zapbrew.fish").is_file(),
        "source must survive"
    );
}

#[test]
fn link_refuses_to_overwrite_unrelated_files() {
    let tmp = TempDir::new().expect("temp dir");
    let prefix = tmp.path();
    let bash_dir = prefix.join("etc/bash_completion.d");
    std::fs::create_dir_all(&bash_dir).expect("create bash dir");
    let existing = bash_dir.join("zapbrew");
    std::fs::write(&existing, "foreign").expect("write existing");

    let mut cmd = Command::cargo_bin("zapbrew").expect("binary");
    cmd.env("HOMEBREW_PREFIX", prefix)
        .env("HOMEBREW_REPOSITORY", prefix)
        .env("HOMEBREW_API_DOMAIN", "http://localhost:1")
        .args(["completions", "link"]);
    let output = cmd.output().expect("run link");
    assert!(!output.status.success(), "link must fail on conflict");

    let content = std::fs::read_to_string(&existing).expect("read existing");
    assert_eq!(
        content, "foreign",
        "existing file content must be unchanged"
    );
    assert!(
        !prefix.join("completions/bash/zapbrew").exists(),
        "source installation must not begin after a conflict"
    );
}

#[test]
fn link_discovers_installed_tap_completion_files() {
    let tmp = TempDir::new().expect("temp dir");
    let tap = tmp
        .path()
        .join("Library/Taps/acme/homebrew-tools/completions");
    for (shell, file) in [
        ("bash", "brew-tools"),
        ("zsh", "_brew-tools"),
        ("fish", "brew-tools.fish"),
    ] {
        let dir = tap.join(shell);
        std::fs::create_dir_all(&dir).expect("create tap completion dir");
        std::fs::write(dir.join(file), format!("{shell} completion"))
            .expect("write tap completion");
    }

    let mut cmd = Command::cargo_bin("zapbrew").expect("binary");
    cmd.env("HOMEBREW_PREFIX", tmp.path())
        .env("HOMEBREW_REPOSITORY", tmp.path())
        .env("HOMEBREW_API_DOMAIN", "http://localhost:1")
        .args(["completions", "link"]);
    let output = cmd.output().expect("run link");
    assert!(output.status.success(), "tap completion link exit status");

    for destination in [
        "etc/bash_completion.d/brew-tools",
        "share/zsh/site-functions/_brew-tools",
        "share/fish/vendor_completions.d/brew-tools.fish",
    ] {
        assert!(tmp.path().join(destination).is_symlink(), "{destination}");
    }
}

#[test]
fn link_refuses_while_completion_management_lock_is_held() {
    let tmp = TempDir::new().expect("temp dir");
    let locks_path = tmp.path().join("var/homebrew/locks");
    let locks = Utf8Path::from_path(&locks_path).expect("UTF-8 temp path");
    let _lock = LockGuard::acquire(locks, "completions.lock").expect("hold lock");

    let mut link = Command::cargo_bin("zapbrew").expect("binary");
    link.env("HOMEBREW_PREFIX", tmp.path())
        .env("HOMEBREW_REPOSITORY", tmp.path())
        .env("HOMEBREW_API_DOMAIN", "http://localhost:1")
        .args(["completions", "link"]);
    let output = link.output().expect("run link");
    assert!(!output.status.success(), "contended link must fail");
    assert!(!tmp.path().join("completions").exists());
    assert!(!tmp.path().join("etc/bash_completion.d/zapbrew").exists());
}

#[cfg(unix)]
#[test]
fn link_rejects_symlinked_destination_ancestor_before_mutation() {
    use std::os::unix::fs::symlink;

    let tmp = TempDir::new().expect("temp dir");
    let outside = TempDir::new().expect("outside temp dir");
    symlink(outside.path(), tmp.path().join("share")).expect("symlink share");

    let mut cmd = Command::cargo_bin("zapbrew").expect("binary");
    cmd.env("HOMEBREW_PREFIX", tmp.path())
        .env("HOMEBREW_REPOSITORY", tmp.path())
        .env("HOMEBREW_API_DOMAIN", "http://localhost:1")
        .args(["completions", "link"]);
    let output = cmd.output().expect("run link");
    assert!(!output.status.success(), "symlink ancestor must be refused");
    assert!(
        !tmp.path().join("completions/bash/zapbrew").exists(),
        "source installation must not begin"
    );
    assert_eq!(
        std::fs::read_dir(outside.path())
            .expect("read outside")
            .count(),
        0,
        "completion link must not escape through the ancestor symlink"
    );
}
