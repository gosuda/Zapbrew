//! End-to-end tests for `zapbrew_pour::relocate` against a real on-disk keg.
//!
//! Byte-level ELF/Mach-O structural assertions live in the crate's private
//! `#[cfg(test)]` module (they need `object` and the private decode seams);
//! these cover the public contract: text substitution, hardlink preservation,
//! the `:any_skip_relocation` short-circuit, and the plan-before-write
//! guarantee that a rejected mutation leaves every file byte-unchanged.

use std::collections::HashMap;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::str::FromStr;

use camino::{Utf8Path, Utf8PathBuf};
use tempfile::TempDir;
use zapbrew_prefix::{CommandOutput, CommandRunner, CommandSpec, Env, EnvDetectInput, Keg};
use zapbrew_types::{FormulaName, PkgVersion};

/// `unwrap`/`expect` are denied workspace-wide.
fn okr<T, E: std::fmt::Debug>(value: Result<T, E>, message: &str) -> T {
    match value {
        Ok(inner) => inner,
        Err(error) => panic!("{message}: {error:?}"),
    }
}

/// Runner that must never be invoked (no macOS host tools in these tests).
struct PanicRunner;

impl CommandRunner for PanicRunner {
    fn run(&self, _spec: &CommandSpec) -> Result<CommandOutput, std::io::Error> {
        panic!("relocate must not spawn a subprocess for ELF/text kegs");
    }
}

fn utf8(temp: &TempDir) -> Utf8PathBuf {
    match Utf8PathBuf::from_path_buf(temp.path().to_path_buf()) {
        Ok(path) => path,
        Err(_) => panic!("temp dir is not UTF-8"),
    }
}

/// Build an Env whose prefix/cellar point into `root`; `prefix` overrides the
/// installed prefix (kept separate from the on-disk cellar).
fn make_env(root: &Utf8Path, prefix: &str) -> Env {
    let cellar = root.join("cellar");
    let mut vars = HashMap::new();
    vars.insert("HOMEBREW_PREFIX".to_owned(), prefix.to_owned());
    vars.insert("HOMEBREW_CELLAR".to_owned(), cellar.to_string());
    let input = EnvDetectInput {
        os: "linux".to_owned(),
        arch: "x86_64".to_owned(),
        home: root.join("home"),
        xdg_cache_home: None,
        vars,
        available_parallelism: 2,
    };
    okr(Env::detect_from(&input, &PanicRunner), "detect env")
}

fn make_keg(env: &Env) -> Keg {
    let name = okr(FormulaName::from_str("foo"), "name");
    let version = okr(PkgVersion::from_str("1.0"), "version");
    let keg = okr(Keg::new(&env.cellar, name, version), "keg");
    okr(
        fs::create_dir_all(keg.path().as_std_path()),
        "create keg dir",
    );
    keg
}

fn write_file(keg: &Keg, relative: &str, bytes: &[u8]) -> Utf8PathBuf {
    let path = keg.path().join(relative);
    if let Some(parent) = path.parent() {
        okr(fs::create_dir_all(parent.as_std_path()), "mkdir");
    }
    okr(fs::write(path.as_std_path(), bytes), "write file");
    path
}

fn read(path: &Utf8Path) -> Vec<u8> {
    okr(fs::read(path.as_std_path()), "read")
}

fn inode(path: &Utf8Path) -> u64 {
    okr(fs::metadata(path.as_std_path()), "stat").ino()
}

#[test]
fn text_files_substitute_placeholders_and_report_relative_paths() {
    let temp = okr(TempDir::new(), "temp");
    let root = utf8(&temp);
    let env = make_env(&root, "/opt/hb");
    let keg = make_keg(&env);

    let cfg = write_file(&keg, "share/x.cfg", b"dir=@@HOMEBREW_PREFIX@@/etc\n");

    let report = okr(relocate_call(&keg, &env, ":any"), "relocate");
    assert!(
        report
            .changed_files
            .iter()
            .any(|p| p.as_str() == "share/x.cfg"),
        "report should list the relative path"
    );
    assert_eq!(read(&cfg), b"dir=/opt/hb/etc\n");
}

#[test]
fn hardlinked_files_are_rewritten_once_and_relinked() {
    let temp = okr(TempDir::new(), "temp");
    let root = utf8(&temp);
    let env = make_env(&root, "/opt/hb");
    let keg = make_keg(&env);

    let primary = write_file(&keg, "bin/a", b"p=@@HOMEBREW_PREFIX@@/bin\n");
    let secondary = keg.path().join("bin/b");
    okr(
        fs::hard_link(primary.as_std_path(), secondary.as_std_path()),
        "hardlink",
    );
    assert_eq!(
        inode(&primary),
        inode(&secondary),
        "precondition: shared inode"
    );

    let report = okr(relocate_call(&keg, &env, ":any"), "relocate");

    // Only the representative (sorted first) is reported.
    assert!(report.changed_files.iter().any(|p| p.as_str() == "bin/a"));
    assert!(!report.changed_files.iter().any(|p| p.as_str() == "bin/b"));

    // Both links carry the substituted content and still share one inode.
    assert_eq!(read(&primary), b"p=/opt/hb/bin\n");
    assert_eq!(read(&secondary), b"p=/opt/hb/bin\n");
    assert_eq!(inode(&primary), inode(&secondary), "hardlink preserved");
}

#[test]
fn any_skip_relocation_leaves_files_untouched() {
    let temp = okr(TempDir::new(), "temp");
    let root = utf8(&temp);
    let env = make_env(&root, "/opt/hb");
    let keg = make_keg(&env);

    let original = b"dir=@@HOMEBREW_PREFIX@@/etc\n";
    let cfg = write_file(&keg, "share/x.cfg", original);

    let report = okr(
        relocate_call(&keg, &env, ":any_skip_relocation"),
        "relocate",
    );
    assert!(report.changed_files.is_empty());
    assert_eq!(read(&cfg), original, "skip must not rewrite anything");
}

#[test]
fn capacity_failure_leaves_every_file_byte_unchanged() {
    let temp = okr(TempDir::new(), "temp");
    let root = utf8(&temp);
    // A prefix far longer than the 19-byte placeholder forces the binary NUL
    // slot to overflow, which must reject the whole keg before any write.
    let env = make_env(&root, "/a/very/long/custom/homebrew/prefix/location/deep");
    let keg = make_keg(&env);

    let binary_original: &[u8] = b"\x00@@HOMEBREW_PREFIX@@/lib\x00rest\x00";
    let text_original: &[u8] = b"dir=@@HOMEBREW_PREFIX@@/etc\n";
    let binary = write_file(&keg, "lib/x.bin", binary_original);
    let text = write_file(&keg, "share/x.cfg", text_original);

    let result = relocate_call(&keg, &env, ":any");
    assert!(result.is_err(), "oversized replacement must be rejected");

    // Plan-before-write: nothing was promoted, so both files are unchanged.
    assert_eq!(read(&binary), binary_original);
    assert_eq!(read(&text), text_original);
}

fn relocate_call(
    keg: &Keg,
    env: &Env,
    cellar_field: &str,
) -> Result<zapbrew_pour::RelocationReport, zapbrew_pour::PourError> {
    zapbrew_pour::relocate(keg, env, cellar_field, Some("6.0.14-zapbrew"), &PanicRunner)
}
