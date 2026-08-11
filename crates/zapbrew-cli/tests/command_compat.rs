//! Process-boundary command-compatibility contracts.
//!
//! Five tests drive the real `zapbrew` binary at the OS process boundary. The
//! command-reference case pins all 44 generated help blocks; the other four
//! pin exact bytes and exit status. The unknown-formula case
//! traverses the production JWS verifier: `cargo test -p zapbrew-cli` unifies
//! the `zapbrew-api` `test-trust-root` dev-dependency feature into the binary
//! built for this test (resolver 3), so the embedded trust anchor is the
//! checked-in test root and a fixture signed by it verifies through the
//! unchanged PS512/RFC7797 path. The verified fresh-cache path
//! (`HOMEBREW_NO_AUTO_UPDATE=1` plus a pre-seeded, verified cache) short-circuits
//! before any HTTP request, so the run is fully offline and deterministic.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::time::Duration;

use assert_cmd::Command;
use tempfile::TempDir;

/// Envelopes signed by the dedicated test trust root: payload `[]` for the
/// formula catalog and `{}` for tap migrations. Single source of truth lives in
/// the API crate's testdata (dependency direction: cli -> api); both are seeded
/// into the temp cache so the missing-formula path never reaches the network.
const FORMULA_JWS: &[u8] = include_bytes!("../../zapbrew-api/testdata/formula.jws.json");
const MIGRATIONS_JWS: &[u8] =
    include_bytes!("../../zapbrew-api/testdata/formula_tap_migrations.jws.json");

#[derive(Debug)]
struct DocumentedCommand {
    args: Vec<String>,
    help: String,
}

fn fenced_block(lines: &[&str], heading: usize) -> String {
    let start = lines[heading..]
        .iter()
        .position(|line| *line == "```text")
        .map(|offset| heading + offset + 1)
        .expect("text fence after command heading");
    let end = lines[start..]
        .iter()
        .position(|line| *line == "```")
        .map(|offset| start + offset)
        .expect("closing text fence");
    format!("{}\n", lines[start..end].join("\n"))
}

fn documented_commands(reference: &str) -> Vec<DocumentedCommand> {
    let lines: Vec<&str> = reference.lines().collect();
    lines
        .iter()
        .enumerate()
        .filter_map(|(index, line)| {
            line.strip_prefix("### `zapbrew ")
                .or_else(|| line.strip_prefix("#### `zapbrew "))
                .and_then(|command| command.strip_suffix('`'))
                .map(|command| DocumentedCommand {
                    args: command.split_whitespace().map(str::to_owned).collect(),
                    help: fenced_block(&lines, index),
                })
        })
        .collect()
}

fn binary_help(args: &[String]) -> String {
    let output = Command::cargo_bin("zapbrew")
        .expect("binary")
        .args(args)
        .arg("--help")
        .output()
        .expect("run help");
    assert!(
        output.status.success(),
        "args: {args:?}; status: {:?}",
        output.status
    );
    assert!(output.stderr.is_empty(), "stderr: {:?}", output.stderr);
    String::from_utf8(output.stdout).expect("UTF-8 help")
}

fn child_commands(help: &str) -> Vec<&str> {
    let Some((_, remainder)) = help.split_once("Commands:\n") else {
        return Vec::new();
    };
    let commands = remainder
        .split_once("\n\n")
        .map_or(remainder, |(commands, _)| commands);
    commands
        .lines()
        .filter_map(|line| {
            let name = line.split_whitespace().next()?;
            (name != "help").then_some(name)
        })
        .collect()
}

fn binary_help_tree(root_help: &str) -> BTreeMap<Vec<String>, String> {
    let mut pending: Vec<Vec<String>> = child_commands(root_help)
        .into_iter()
        .map(|name| vec![name.to_owned()])
        .collect();
    let mut tree = BTreeMap::new();
    while let Some(path) = pending.pop() {
        let help = binary_help(&path);
        for child in child_commands(&help) {
            let mut child_path = path.clone();
            child_path.push(child.to_owned());
            pending.push(child_path);
        }
        assert!(
            tree.insert(path, help).is_none(),
            "duplicate command path in help tree"
        );
    }
    tree
}

#[test]
fn command_reference_matches_binary_help() {
    const REFERENCE: &str = include_str!("../../../docs/commands.md");

    let documented = documented_commands(REFERENCE);
    assert_eq!(documented.len(), 44, "documented command help blocks");

    let root_help = binary_help(&[]);
    let actual_global = root_help
        .split_once("\nOptions:\n")
        .map(|(_, options)| format!("Options:\n{options}"))
        .expect("root Options section");
    let reference_lines: Vec<&str> = REFERENCE.lines().collect();
    let global_heading = reference_lines
        .iter()
        .position(|line| *line == "## Global options and path queries")
        .expect("global options heading");
    assert_eq!(
        fenced_block(&reference_lines, global_heading),
        actual_global
    );

    let actual = binary_help_tree(&root_help);
    let documented_paths: BTreeSet<Vec<String>> = documented
        .iter()
        .map(|command| command.args.clone())
        .collect();
    let actual_paths: BTreeSet<Vec<String>> = actual.keys().cloned().collect();
    assert_eq!(documented_paths, actual_paths);

    for command in documented {
        assert_eq!(
            command.help,
            *actual
                .get(&command.args)
                .expect("documented command exists in binary help tree"),
            "stale help for zapbrew {}",
            command.args.join(" ")
        );
    }
}

#[test]
fn version_is_homebrew_compatible() {
    let output = Command::cargo_bin("zapbrew")
        .expect("binary")
        .arg("--version")
        .output()
        .expect("run --version");

    assert!(output.status.success(), "status: {:?}", output.status);
    assert_eq!(
        output.stdout,
        format!(
            "zapbrew {} (Homebrew 5-compatible)\n",
            env!("CARGO_PKG_VERSION")
        )
        .into_bytes()
    );
    assert!(output.stderr.is_empty(), "stderr: {:?}", output.stderr);
}

#[test]
fn bare_prefix_prints_configured_prefix() {
    let tmp = TempDir::new().expect("tempdir");
    let prefix = tmp.path().join("prefix");

    let output = Command::cargo_bin("zapbrew")
        .expect("binary")
        .env_clear()
        .env("HOME", tmp.path())
        .env("HOMEBREW_PREFIX", &prefix)
        .arg("--prefix")
        .output()
        .expect("run --prefix");

    assert!(output.status.success(), "status: {:?}", output.status);
    let mut expected = prefix.into_os_string().into_string().expect("utf8 prefix");
    expected.push('\n');
    assert_eq!(output.stdout, expected.into_bytes());
    assert!(output.stderr.is_empty(), "stderr: {:?}", output.stderr);
}

#[test]
fn unknown_command_is_clap_usage_error() {
    let output = Command::cargo_bin("zapbrew")
        .expect("binary")
        .arg("frobnicate")
        .output()
        .expect("run unknown command");

    assert_eq!(output.status.code(), Some(2), "status: {:?}", output.status);
    assert!(output.stdout.is_empty(), "stdout: {:?}", output.stdout);
    assert_eq!(
        output.stderr,
        b"error: unrecognized subcommand 'frobnicate'\n\nUsage: zapbrew [OPTIONS] [COMMAND]\n\nFor more information, try '--help'.\n"
    );
}

#[test]
fn unknown_formula_traverses_real_verifier_offline() {
    let tmp = TempDir::new().expect("tempdir");
    let cache = tmp.path().join("cache");
    let api = cache.join("api");
    fs::create_dir_all(&api).expect("create cache/api");
    fs::write(api.join("formula.jws.json"), FORMULA_JWS).expect("seed formula cache");
    fs::write(api.join("formula_tap_migrations.jws.json"), MIGRATIONS_JWS)
        .expect("seed migrations cache");

    let mut cmd = Command::cargo_bin("zapbrew").expect("binary");
    cmd.env_clear()
        .env("HOME", tmp.path())
        .env("HOMEBREW_PREFIX", tmp.path().join("prefix"))
        .env("HOMEBREW_CACHE", &cache)
        .env("HOMEBREW_NO_AUTO_UPDATE", "1")
        // No network is contacted: the verified fresh cache short-circuits before
        // any HTTP request. The timeout only bounds a regression that reached out.
        .timeout(Duration::from_secs(30))
        .args(["install", "nope"]);
    let output = cmd.output().expect("run install nope");

    assert_eq!(output.status.code(), Some(1), "status: {:?}", output.status);
    assert!(output.stdout.is_empty(), "stdout: {:?}", output.stdout);
    assert_eq!(
        output.stderr,
        b"Error: No available formula with the name \"nope\".\n"
    );
}
