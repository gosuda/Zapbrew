//! Integration tests for the public `zapbrew-net` surface.
//!
//! Covers the re-export wiring in `lib.rs` and the public contract the
//! `download` module builds against: bottle tag selection (`select_bottle`)
//! and the request/result types (`CachedBottle`, `DownloadRequest`).
//!
//! Cache internals (`CachePaths`, `bottle_basename`, `validate_segment`,
//! `relative_alias_target`, `checksum_file`, `publish`) are crate-private and
//! therefore unit-tested in `src/cache.rs`.

use std::collections::HashMap;
use std::str::FromStr;

use camino::Utf8PathBuf;
use tempfile::TempDir;
use zapbrew_net::{CachedBottle, DownloadRequest, NetError, select_bottle};
use zapbrew_prefix::{CommandOutput, CommandRunner, CommandSpec, Env, EnvDetectInput};
use zapbrew_types::{Arch, BottleFile, BottleTag, Checksum, FormulaName, MacOsVersion, PkgVersion};

struct PanicRunner;
impl CommandRunner for PanicRunner {
    fn run(&self, _spec: &CommandSpec) -> Result<CommandOutput, std::io::Error> {
        panic!("command runner should not be invoked for linux detect_from");
    }
}

/// Build an `Env` whose `bottle_tag` is pinned to `tag`, with all paths under
/// a tempdir so selection never touches real Homebrew state.
fn test_env(tag: BottleTag) -> (TempDir, Env) {
    let dir = TempDir::new().expect("tempdir");
    let home = Utf8PathBuf::from_path_buf(dir.path().to_path_buf()).expect("utf8");
    let cache = home.join("cache");
    let mut vars = HashMap::new();
    vars.insert("HOMEBREW_CACHE".to_owned(), cache.as_str().to_owned());
    vars.insert(
        "HOMEBREW_PREFIX".to_owned(),
        home.join("prefix").as_str().to_owned(),
    );
    let input = EnvDetectInput {
        os: "linux".to_owned(),
        arch: "x86_64".to_owned(),
        home,
        xdg_cache_home: None,
        vars,
        available_parallelism: 2,
    };
    let mut env = Env::detect_from(&input, &PanicRunner).expect("env");
    env.bottle_tag = tag;
    (dir, env)
}

fn bottle(tag: &str, sha: &str) -> BottleFile {
    BottleFile {
        tag: BottleTag::from_str(tag).expect("tag"),
        cellar: "any".into(),
        url: format!("https://example.test/{tag}"),
        sha256: Checksum::from_str(sha).expect("sha"),
    }
}

const SHA: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const SHA2: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const SHA3: &str = "cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";

#[test]
fn selects_exact_linux_tag() {
    let (_dir, env) = test_env(BottleTag::Linux { arch: Arch::X86_64 });
    let name = FormulaName::from_str("wget").expect("name");
    let files = vec![
        bottle("arm64_linux", SHA),
        bottle("x86_64_linux", SHA2),
        bottle("all", SHA3),
    ];
    let selected = select_bottle(&env, &name, &files).expect("select");
    assert_eq!(selected.tag, BottleTag::Linux { arch: Arch::X86_64 });
    assert_eq!(selected.sha256.as_str(), SHA2);
}

#[test]
fn falls_back_to_older_macos_descending() {
    let (_dir, env) = test_env(BottleTag::MacOs {
        arch: Arch::Arm64,
        version: MacOsVersion::Sonoma,
    });
    let name = FormulaName::from_str("wget").expect("name");
    let files = vec![
        bottle("arm64_monterey", SHA),
        bottle("arm64_ventura", SHA2),
        bottle("all", SHA3),
    ];
    let selected = select_bottle(&env, &name, &files).expect("select");
    assert_eq!(
        selected.tag,
        BottleTag::MacOs {
            arch: Arch::Arm64,
            version: MacOsVersion::Ventura,
        }
    );
    assert_eq!(selected.sha256.as_str(), SHA2);
}

#[test]
fn falls_back_to_all() {
    let (_dir, env) = test_env(BottleTag::Linux { arch: Arch::X86_64 });
    let name = FormulaName::from_str("wget").expect("name");
    let files = vec![bottle("arm64_linux", SHA), bottle("all", SHA2)];
    let selected = select_bottle(&env, &name, &files).expect("select");
    assert_eq!(selected.tag, BottleTag::All);
}

#[test]
fn no_bottle_returns_error_with_brew_suggestion() {
    let (_dir, env) = test_env(BottleTag::Linux { arch: Arch::X86_64 });
    let name = FormulaName::from_str("wget").expect("name");
    let files = vec![bottle("arm64_linux", SHA)];
    let err = select_bottle(&env, &name, &files).expect_err("no bottle");
    assert!(matches!(
        err,
        NetError::NoBottle {
            ref name,
            ref tag,
        } if name == "wget" && *tag == BottleTag::Linux { arch: Arch::X86_64 }
    ));
    // The reporter owns the `Error:` prefix; the message itself must not
    // double it, and must name brew's source-build escape hatch.
    let msg = err.to_string();
    assert!(!msg.starts_with("Error:"), "got {msg}");
    assert_eq!(
        msg,
        "wget: no bottle available for x86_64_linux. brew can build from source; zapbrew cannot."
    );
}

#[test]
fn request_and_result_types_are_public() {
    let name = FormulaName::from_str("wget").expect("name");
    let file = bottle("x86_64_linux", SHA2);
    let pkg_version = PkgVersion::from_str("1.25.0").expect("ver");

    let request = DownloadRequest {
        name,
        bottle: file,
        pkg_version,
        rebuild: 2,
    };
    assert_eq!(request.name.as_str(), "wget");
    assert_eq!(request.rebuild, 2);

    let path = Utf8PathBuf::from(format!(
        "/cache/downloads/{hash}--wget--1.25.0.x86_64_linux.bottle.2.tar.gz",
        hash = "e".repeat(64)
    ));
    let alias = Utf8PathBuf::from("/cache/wget--1.25.0.x86_64_linux.bottle.2.tar.gz");
    let result = CachedBottle {
        path: path.clone(),
        alias: alias.clone(),
        reused: true,
    };
    assert_eq!(result.path, path);
    assert_eq!(result.alias, alias);
    assert!(result.reused);
}
