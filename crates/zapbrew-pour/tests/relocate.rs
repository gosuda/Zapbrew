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

// ---- fix_dynamic_linkage parity: symlink relativeization ----

/// Read a symlink target as a UTF-8 string.
fn read_link(path: &Utf8Path) -> String {
    let target = okr(fs::read_link(path.as_std_path()), "readlink");
    target.to_string_lossy().into_owned()
}

/// Create a symlink inside the keg at `relative` pointing to `target`.
fn write_symlink(keg: &Keg, relative: &str, target: &str) -> Utf8PathBuf {
    use std::os::unix::fs::symlink;
    let path = keg.path().join(relative);
    if let Some(parent) = path.parent() {
        okr(fs::create_dir_all(parent.as_std_path()), "mkdir");
    }
    okr(symlink(target, path.as_std_path()), "symlink");
    path
}

#[test]
fn absolute_symlink_to_cellar_relativeized() {
    let temp = okr(TempDir::new(), "temp");
    let root = utf8(&temp);
    let env = make_env(&root, "/opt/hb");
    let keg = make_keg(&env);

    // Symlink in bin/ pointing at an absolute path inside the keg's lib/.
    let target = format!("{}/lib/libfoo.so", keg.path());
    let link = write_symlink(&keg, "bin/tool", &target);

    okr(relocate_call(&keg, &env, ":any"), "relocate");

    // The symlink should now be a relative path.
    let new_target = read_link(&link);
    assert!(
        !new_target.starts_with('/'),
        "symlink target should be relative, got {new_target}"
    );
    assert!(
        new_target.ends_with("lib/libfoo.so"),
        "relative target should end with the basename, got {new_target}"
    );
}

#[test]
fn absolute_symlink_to_prefix_relativeized() {
    let temp = okr(TempDir::new(), "temp");
    let root = utf8(&temp);
    let env = make_env(&root, "/opt/hb");
    let keg = make_keg(&env);

    // Symlink pointing at an absolute path under the prefix (not the cellar).
    let link = write_symlink(&keg, "bin/tool", "/opt/hb/libexec/tool");

    okr(relocate_call(&keg, &env, ":any"), "relocate");

    let new_target = read_link(&link);
    assert!(
        !new_target.starts_with('/'),
        "symlink target should be relative, got {new_target}"
    );
    assert!(
        new_target.ends_with("libexec/tool"),
        "relative target should end with the basename, got {new_target}"
    );
}

#[test]
fn placeholder_symlink_target_substituted_then_relativeized() {
    let temp = okr(TempDir::new(), "temp");
    let root = utf8(&temp);
    let env = make_env(&root, "/opt/hb");
    let keg = make_keg(&env);

    // Symlink whose target contains a placeholder — must be substituted
    // before relativeization.
    let link = write_symlink(&keg, "bin/tool", "@@HOMEBREW_PREFIX@@/libexec/tool");

    okr(relocate_call(&keg, &env, ":any"), "relocate");

    let new_target = read_link(&link);
    assert!(
        !new_target.starts_with('/'),
        "symlink target should be relative, got {new_target}"
    );
    assert!(
        new_target.ends_with("libexec/tool"),
        "relative target should end with the basename, got {new_target}"
    );
}

#[test]
fn foreign_absolute_symlink_left_untouched() {
    let temp = okr(TempDir::new(), "temp");
    let root = utf8(&temp);
    let env = make_env(&root, "/opt/hb");
    let keg = make_keg(&env);

    // Symlink pointing outside the prefix/cellar — must NOT be rewritten.
    let link = write_symlink(&keg, "bin/foreign", "/usr/bin/env");

    okr(relocate_call(&keg, &env, ":any"), "relocate");

    assert_eq!(read_link(&link), "/usr/bin/env");
}

#[test]
fn already_relative_symlink_left_untouched() {
    let temp = okr(TempDir::new(), "temp");
    let root = utf8(&temp);
    let env = make_env(&root, "/opt/hb");
    let keg = make_keg(&env);

    let link = write_symlink(&keg, "lib/libfoo.so", "../lib/libfoo_real.so");

    okr(relocate_call(&keg, &env, ":any"), "relocate");

    assert_eq!(read_link(&link), "../lib/libfoo_real.so");
}

#[test]
fn unpack_then_relocate_rewrites_absolute_build_cellar_symlink() {
    use flate2::Compression;
    use flate2::write::GzEncoder;
    use tar::{Builder, Header};
    use zapbrew_pour::unpack;
    use zapbrew_types::{FormulaName, PkgVersion};

    let temp = okr(TempDir::new(), "temp");
    let root = utf8(&temp);
    let env = make_env(&root, "/opt/hb");
    let cellar = env.cellar.clone();

    // Build a bottle tarball with an absolute symlink into a build-time
    // cellar that differs from the user's cellar.
    let tarball = root.join("test.bottle.tar.gz");
    let file = okr(
        std::fs::File::create(tarball.as_std_path()),
        "create tarball",
    );
    let encoder = GzEncoder::new(file, Compression::default());
    let mut builder = Builder::new(encoder);

    // Directory entries
    let mut dir_header = Header::new_gnu();
    dir_header.set_path("foo/1.0.0").expect("set_path");
    dir_header.set_entry_type(tar::EntryType::Directory);
    dir_header.set_mode(0o755);
    dir_header.set_size(0);
    dir_header.set_cksum();
    builder
        .append(&dir_header, std::io::empty())
        .expect("append dir");

    let mut dir_header = Header::new_gnu();
    dir_header.set_path("foo/1.0.0/lib").expect("set_path");
    dir_header.set_entry_type(tar::EntryType::Directory);
    dir_header.set_mode(0o755);
    dir_header.set_size(0);
    dir_header.set_cksum();
    builder
        .append(&dir_header, std::io::empty())
        .expect("append lib dir");

    // Real file
    let data = b"fake-dylib\n";
    let mut file_header = Header::new_gnu();
    file_header.set_mode(0o644);
    file_header.set_size(data.len() as u64);
    file_header.set_cksum();
    builder
        .append_data(&mut file_header, "foo/1.0.0/lib/libfoo.dylib", &data[..])
        .expect("append file");

    // Absolute symlink into a build cellar (not the user's cellar)
    let mut link_header = Header::new_gnu();
    link_header.set_entry_type(tar::EntryType::Symlink);
    link_header.set_mode(0o777);
    link_header.set_size(0);
    link_header
        .set_link_name("@@HOMEBREW_CELLAR@@/foo/1.0.0/lib/libfoo.dylib")
        .expect("set_link_name");
    link_header.set_cksum();
    builder
        .append_data(
            &mut link_header,
            "foo/1.0.0/lib/libbar.dylib",
            std::io::empty(),
        )
        .expect("append symlink");

    let encoder = okr(builder.into_inner(), "finish tar");
    okr(encoder.finish(), "finish gzip");

    let name = okr(FormulaName::from_str("foo"), "name");
    let version = okr(PkgVersion::from_str("1.0.0"), "version");
    let keg = okr(unpack(&tarball, &cellar, &name, &version), "unpack");

    // After unpack, the symlink is preserved with its original absolute target.
    let link = keg.path().join("lib/libbar.dylib");
    let meta = okr(fs::symlink_metadata(link.as_std_path()), "stat symlink");
    assert!(
        meta.file_type().is_symlink(),
        "libbar.dylib must be a symlink after unpack"
    );
    let pre_target = read_link(&link);
    assert_eq!(
        pre_target, "@@HOMEBREW_CELLAR@@/foo/1.0.0/lib/libfoo.dylib",
        "placeholder target must be preserved through unpack"
    );

    // After relocate, the symlink is rewritten to a relative path.
    okr(relocate_call(&keg, &env, ":any"), "relocate");

    let post_target = read_link(&link);
    assert!(
        !post_target.starts_with('/'),
        "symlink target should be relative after relocate, got {post_target}"
    );
    assert!(
        post_target.ends_with("libfoo.dylib"),
        "relative target should end with libfoo.dylib, got {post_target}"
    );
}

fn relocate_call(
    keg: &Keg,
    env: &Env,
    cellar_field: &str,
) -> Result<zapbrew_pour::RelocationReport, zapbrew_pour::PourError> {
    zapbrew_pour::relocate(keg, env, cellar_field, Some("6.0.14-zapbrew"), &PanicRunner)
}
