//! Integration regressions: domain types may parse path-control-looking values,
//! but filesystem constructor seams must reject them as [`PrefixError::InvalidPathSegment`].

use std::str::FromStr;

use camino::Utf8PathBuf;
use tempfile::TempDir;
use zapbrew_prefix::{Keg, LockGuard, PrefixError, Rack, is_pinned, linked_path, opt_path, unpin};
use zapbrew_types::{FormulaName, PkgVersion};

fn utf8_temp() -> (TempDir, Utf8PathBuf) {
    let temp = match TempDir::new() {
        Ok(temp) => temp,
        Err(err) => panic!("tempdir: {err}"),
    };
    let root = match Utf8PathBuf::from_path_buf(temp.path().to_path_buf()) {
        Ok(root) => root,
        Err(path) => panic!("non-utf8 temp path {}", path.display()),
    };
    (temp, root)
}

fn parse_formula_name(raw: &str) -> FormulaName {
    match FormulaName::from_str(raw) {
        Ok(name) => name,
        Err(err) => panic!("formula name {raw:?} should parse: {err}"),
    }
}

fn parse_pkg_version(raw: &str) -> PkgVersion {
    match PkgVersion::from_str(raw) {
        Ok(version) => version,
        Err(err) => panic!("pkg version {raw:?} should parse: {err}"),
    }
}

fn assert_invalid_path_segment(err: PrefixError, expected_value: &str) {
    match err {
        PrefixError::InvalidPathSegment { value, .. } => {
            assert_eq!(value, expected_value);
        }
        other => panic!("expected InvalidPathSegment for {expected_value:?}, got {other}"),
    }
}

fn assert_formula_name_filesystem_seams_reject(name: &FormulaName, raw: &str) {
    let (_temp, root) = utf8_temp();
    let cellar = root.join("Cellar");
    let linked_dir = root.join("var/homebrew/linked");
    let prefix = root.join("prefix");
    let pins_dir = root.join("var/homebrew/pinned");
    let version = parse_pkg_version("1.0.0");

    match Rack::new(&cellar, name) {
        Err(err) => assert_invalid_path_segment(err, raw),
        Ok(_) => panic!("Rack::new should reject formula name {raw:?}"),
    }

    match linked_path(&linked_dir, name) {
        Err(err) => assert_invalid_path_segment(err, raw),
        Ok(_) => panic!("linked_path should reject formula name {raw:?}"),
    }

    match opt_path(&prefix, name) {
        Err(err) => assert_invalid_path_segment(err, raw),
        Ok(_) => panic!("opt_path should reject formula name {raw:?}"),
    }

    match unpin(&pins_dir, name) {
        Err(err) => assert_invalid_path_segment(err, raw),
        Ok(()) => panic!("unpin should reject formula name {raw:?}"),
    }

    match is_pinned(&pins_dir, name, &version) {
        Err(err) => assert_invalid_path_segment(err, raw),
        Ok(_) => panic!("is_pinned should reject formula name {raw:?}"),
    }
}

fn assert_lock_acquire_rejects(lock_file_name: &str) {
    let (_temp, root) = utf8_temp();
    let locks_dir = root.join("var/homebrew/locks");
    let escape_via_traversal = root.join("tmp").join("pwn.lock");

    match LockGuard::acquire(&locks_dir, lock_file_name) {
        Err(err) => assert_invalid_path_segment(err, lock_file_name),
        Ok(_guard) => panic!("LockGuard::acquire should reject lock file {lock_file_name:?}"),
    }

    assert!(
        !escape_via_traversal.exists(),
        "lock acquire must not create {escape_via_traversal}"
    );
    assert!(
        !locks_dir.join(lock_file_name).exists(),
        "lock acquire must not join invalid segment {lock_file_name:?} under locks_dir"
    );
}

#[test]
fn formula_name_dot_and_dotdot_parse_as_domain_values() {
    for raw in [".", ".."] {
        let name = parse_formula_name(raw);
        assert_eq!(name.name(), raw);
        assert_eq!(name.as_str(), raw);
        assert_eq!(name.tap(), None);
    }
}

#[test]
fn formula_name_dot_rejected_at_filesystem_seams() {
    let name = parse_formula_name(".");
    assert_formula_name_filesystem_seams_reject(&name, ".");
}

#[test]
fn formula_name_dotdot_rejected_at_filesystem_seams() {
    let name = parse_formula_name("..");
    assert_formula_name_filesystem_seams_reject(&name, "..");
}

#[test]
fn pkg_version_path_traversal_strings_parse_as_domain_values() {
    for raw in ["../../../tmp/pwn", "../escape"] {
        let version = parse_pkg_version(raw);
        assert_eq!(version.to_string(), raw);
    }
}

#[test]
fn keg_new_rejects_path_traversal_pkg_versions() {
    let (_temp, root) = utf8_temp();
    let cellar = root.join("Cellar");
    let name = parse_formula_name("safe-formula");

    for raw in ["../../../tmp/pwn", "../escape"] {
        let version = parse_pkg_version(raw);
        match Keg::new(&cellar, name.clone(), version) {
            Err(err) => assert_invalid_path_segment(err, raw),
            Ok(_) => panic!("Keg::new should reject pkg version {raw:?}"),
        }
    }
}

#[test]
fn tapped_formula_name_uses_basename_at_filesystem_seams() {
    let (_temp, root) = utf8_temp();
    let cellar = root.join("Cellar");
    let linked_dir = root.join("var/homebrew/linked");
    let prefix = root.join("prefix");
    let name = parse_formula_name("homebrew/core/wget");

    assert_eq!(name.name(), "wget");
    assert_eq!(name.tap(), Some(("homebrew", "core")));

    let rack = match Rack::new(&cellar, &name) {
        Ok(rack) => rack,
        Err(err) => panic!("Rack::new for tapped name: {err}"),
    };
    assert_eq!(rack.path(), cellar.join("wget"));
    assert_eq!(rack.name(), "wget");

    let linked = match linked_path(&linked_dir, &name) {
        Ok(path) => path,
        Err(err) => panic!("linked_path for tapped name: {err}"),
    };
    assert_eq!(linked, linked_dir.join("wget"));

    let opt = match opt_path(&prefix, &name) {
        Ok(path) => path,
        Err(err) => panic!("opt_path for tapped name: {err}"),
    };
    assert_eq!(opt, prefix.join("opt").join("wget"));
}

#[test]
fn lock_acquire_rejects_path_traversal_lock_file_name() {
    assert_lock_acquire_rejects("../../../tmp/pwn.lock");
}

#[test]
fn lock_acquire_rejects_absolute_lock_file_name() {
    assert_lock_acquire_rejects("/tmp/pwn.lock");
}

#[test]
fn lock_acquire_rejects_dot_lock_file_name() {
    assert_lock_acquire_rejects(".");
}

#[test]
fn lock_acquire_rejects_dotdot_lock_file_name() {
    assert_lock_acquire_rejects("..");
}

#[test]
fn keg_new_accepts_normal_revisioned_version() {
    let (_temp, root) = utf8_temp();
    let cellar = root.join("Cellar");
    let name = parse_formula_name("foo");
    let version = parse_pkg_version("1.2.3_1");

    let keg = match Keg::new(&cellar, name.clone(), version.clone()) {
        Ok(keg) => keg,
        Err(err) => panic!("Keg::new for normal version: {err}"),
    };

    assert_eq!(keg.name(), &name);
    assert_eq!(keg.version(), &version);
    assert_eq!(keg.path(), cellar.join("foo").join("1.2.3_1"));
}
