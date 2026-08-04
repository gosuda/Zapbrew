//! End-to-end smoke for the `completions` subcommand.
//!
//! Runs the real `zapbrew` binary per shell and proves the `main` interception
//! path: exit 0, the shell's registration marker on stdout, and nothing on
//! stderr. Completion generation is intercepted before any environment or
//! network work, so this needs no host setup and no fixtures.

use assert_cmd::Command;

/// Assert that `completions <shell>` exits 0, prints `registration` on stdout,
/// and writes nothing to stderr.
fn smoke(shell: &str, registration: &str) {
    let output = Command::cargo_bin("zapbrew")
        .expect("binary")
        .args(["completions", shell])
        .output()
        .expect("run completions");
    assert!(output.status.success(), "{shell} completions exit status");
    assert!(
        output.stderr.is_empty(),
        "{shell} completions wrote to stderr"
    );
    let stdout = String::from_utf8(output.stdout).expect("stdout is UTF-8");
    assert!(
        stdout.contains(registration),
        "{shell} completions missing {registration}"
    );
}

#[test]
fn bash_completions_smoke() {
    smoke("bash", "_zapbrew");
}

#[test]
fn zsh_completions_smoke() {
    smoke("zsh", "#compdef zapbrew");
}

#[test]
fn fish_completions_smoke() {
    smoke("fish", "complete -c zapbrew");
}
