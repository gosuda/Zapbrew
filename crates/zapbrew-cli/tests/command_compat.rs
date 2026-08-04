//! Process-boundary command-compatibility contracts.
//!
//! Four assert_cmd cases pin the exact bytes and exit status of the real
//! `zapbrew` binary at the OS process boundary. The unknown-formula case
//! traverses the production JWS verifier: `cargo test -p zapbrew-cli` unifies
//! the `zapbrew-api` `test-trust-root` dev-dependency feature into the binary
//! built for this test (resolver 3), so the embedded trust anchor is the
//! checked-in test root and a fixture signed by it verifies through the
//! unchanged PS512/RFC7797 path. The verified fresh-cache path
//! (`HOMEBREW_NO_AUTO_UPDATE=1` plus a pre-seeded, verified cache) short-circuits
//! before any HTTP request, so the run is fully offline and deterministic.

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
