//! Integration tests for prefix path helpers under tempfile prefixes.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::Path;
use std::str::FromStr;

use camino::Utf8PathBuf;
use tempfile::TempDir;
use zapbrew_prefix::{
    CommandOutput, CommandRunner, CommandSpec, Env, EnvDetectInput, Keg, LockGuard, Prefix,
    PrefixError, Rack, SystemCommandRunner, is_pinned, linked_path, opt_path, pin,
    pin_relative_target, resolve_linked, resolve_opt, unpin,
};
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

fn pkg_version(raw: &str) -> PkgVersion {
    match PkgVersion::from_str(raw) {
        Ok(version) => version,
        Err(err) => panic!("pkg version {raw}: {err}"),
    }
}

fn formula_name(raw: &str) -> FormulaName {
    match FormulaName::from_str(raw) {
        Ok(name) => name,
        Err(err) => panic!("formula name {raw}: {err}"),
    }
}

fn rack(cellar: &camino::Utf8Path, name: &FormulaName) -> Rack {
    match Rack::new(cellar, name) {
        Ok(rack) => rack,
        Err(err) => panic!("rack {}: {err}", name.name()),
    }
}

fn keg(cellar: &camino::Utf8Path, name: FormulaName, version: PkgVersion) -> Keg {
    match Keg::new(cellar, name.clone(), version.clone()) {
        Ok(keg) => keg,
        Err(err) => panic!("keg {} {version}: {err}", name.name()),
    }
}

fn path_result(result: Result<Utf8PathBuf, PrefixError>, context: &str) -> Utf8PathBuf {
    match result {
        Ok(path) => path,
        Err(err) => panic!("{context}: {err}"),
    }
}

fn bool_result(result: Result<bool, PrefixError>, context: &str) -> bool {
    match result {
        Ok(value) => value,
        Err(err) => panic!("{context}: {err}"),
    }
}

fn create_dir(path: &camino::Utf8Path) {
    if let Err(err) = fs::create_dir_all(path.as_std_path()) {
        panic!("create {}: {err}", path);
    }
}

fn write_symlink(target: &str, link: &camino::Utf8Path) {
    if let Some(parent) = link.parent() {
        create_dir(parent);
    }
    #[cfg(unix)]
    {
        if let Err(err) = std::os::unix::fs::symlink(target, link.as_std_path()) {
            panic!("symlink {} -> {target}: {err}", link);
        }
    }
    #[cfg(windows)]
    {
        if let Err(err) = std::os::windows::fs::symlink_dir(target, link.as_std_path()) {
            panic!("symlink {} -> {target}: {err}", link);
        }
    }
}

fn detect_prefix_env(prefix: &camino::Utf8Path) -> Env {
    let input = EnvDetectInput {
        os: "linux".to_owned(),
        arch: "x86_64".to_owned(),
        home: prefix.to_path_buf(),
        xdg_cache_home: None,
        vars: HashMap::from([
            ("HOMEBREW_PREFIX".to_owned(), prefix.to_string()),
            (
                "HOMEBREW_CELLAR".to_owned(),
                prefix.join("Cellar").to_string(),
            ),
            ("HOMEBREW_TEMP".to_owned(), prefix.join("tmp").to_string()),
        ]),
        available_parallelism: 2,
    };
    match Env::detect_from(&input, &SystemCommandRunner) {
        Ok(env) => env,
        Err(err) => panic!("detect env: {err}"),
    }
}

#[test]
fn symlink_ld_so_creates_link_to_system_loader_on_linux() {
    let (_temp, root) = utf8_temp();
    let prefix = root.join("prefix");
    let env = detect_prefix_env(&prefix);
    let link = prefix.join("lib/ld.so");

    match zapbrew_prefix::symlink_ld_so(&env) {
        Ok(()) => {}
        Err(PrefixError::NoSystemLdSo) => {
            // Host without a discoverable linker (unusual): the contract is a
            // typed refusal, not a half-created link.
            assert!(!link.exists());
            return;
        }
        Err(err) => panic!("symlink_ld_so: {err}"),
    }

    let meta = match link.symlink_metadata() {
        Ok(meta) => meta,
        Err(err) => panic!("link metadata: {err}"),
    };
    assert!(meta.file_type().is_symlink(), "ld.so must be a symlink");
    let target = match fs::read_link(link.as_std_path()) {
        Ok(target) => target,
        Err(err) => panic!("read link: {err}"),
    };
    assert!(
        target.is_absolute() && target.exists(),
        "ld.so must point at an existing loader, got {target:?}"
    );

    // Idempotent: a second run keeps the same link.
    match zapbrew_prefix::symlink_ld_so(&env) {
        Ok(()) => {}
        Err(err) => panic!("second symlink_ld_so: {err}"),
    }
    let after = match fs::read_link(link.as_std_path()) {
        Ok(after) => after,
        Err(err) => panic!("read link after: {err}"),
    };
    assert_eq!(after, target, "existing correct link must be preserved");
}

#[test]
fn symlink_ld_so_is_noop_on_macos() {
    use std::os::unix::process::ExitStatusExt;
    use std::sync::Mutex;

    struct SwVersRunner {
        records: Mutex<usize>,
    }
    impl CommandRunner for SwVersRunner {
        fn run(&self, _spec: &CommandSpec) -> Result<CommandOutput, io::Error> {
            *self.records.lock().expect("records") += 1;
            Ok(CommandOutput::new(
                std::process::ExitStatus::from_raw(0),
                b"15.0\n".to_vec(),
                Vec::new(),
            ))
        }
    }
    let runner = SwVersRunner {
        records: Mutex::new(0),
    };

    let (_temp, root) = utf8_temp();
    let prefix = root.join("prefix");
    let input = EnvDetectInput {
        os: "macos".to_owned(),
        arch: "arm64".to_owned(),
        home: prefix.to_path_buf(),
        xdg_cache_home: None,
        vars: HashMap::from([
            ("HOMEBREW_PREFIX".to_owned(), prefix.to_string()),
            (
                "HOMEBREW_CELLAR".to_owned(),
                prefix.join("Cellar").to_string(),
            ),
            ("HOMEBREW_TEMP".to_owned(), prefix.join("tmp").to_string()),
        ]),
        available_parallelism: 2,
    };
    let env = match Env::detect_from(&input, &runner) {
        Ok(env) => env,
        Err(err) => panic!("detect env: {err}"),
    };
    match zapbrew_prefix::symlink_ld_so(&env) {
        Ok(()) => {}
        Err(err) => panic!("macOS symlink_ld_so: {err}"),
    }
    assert!(
        !prefix.join("lib/ld.so").exists(),
        "macOS must not create a Linux loader link"
    );
}

#[test]
fn rack_all_returns_sorted_nonempty_racks_only() {
    let (_temp, root) = utf8_temp();
    let cellar = root.join("Cellar");

    create_dir(&cellar.join("zeta").join("1.0.0"));
    create_dir(&cellar.join("alpha").join("2.0.0"));
    create_dir(&cellar.join("empty"));
    create_dir(&cellar.join("beta").join("0.1.0"));

    let racks = match Rack::all(&cellar) {
        Ok(racks) => racks,
        Err(err) => panic!("Rack::all: {err}"),
    };

    let names: Vec<&str> = racks.iter().map(Rack::name).collect();
    assert_eq!(names, ["alpha", "beta", "zeta"]);
}

#[test]
fn rack_kegs_returns_sorted_multi_version_kegs() {
    let (_temp, root) = utf8_temp();
    let cellar = root.join("Cellar");
    let rack_path = cellar.join("wget");

    create_dir(&rack_path.join("1.21.4"));
    create_dir(&rack_path.join("1.21.4_1"));
    create_dir(&rack_path.join("1.20.0"));

    let rack = rack(&cellar, &formula_name("wget"));
    let kegs = match rack.kegs() {
        Ok(kegs) => kegs,
        Err(err) => panic!("Rack::kegs: {err}"),
    };

    let versions: Vec<String> = kegs.iter().map(|keg| keg.version().to_string()).collect();
    assert_eq!(versions, ["1.20.0", "1.21.4", "1.21.4_1"]);
    assert!(kegs.iter().all(|keg| keg.name().name() == "wget"));
}

#[test]
fn rack_kegs_orders_semantic_versions_not_lexicographically() {
    let (_temp, root) = utf8_temp();
    let cellar = root.join("Cellar");
    let rack_path = cellar.join("node");

    create_dir(&rack_path.join("1.10.0"));
    create_dir(&rack_path.join("1.9.0"));

    let rack = rack(&cellar, &formula_name("node"));
    let kegs = match rack.kegs() {
        Ok(kegs) => kegs,
        Err(err) => panic!("Rack::kegs: {err}"),
    };

    let versions: Vec<String> = kegs.iter().map(|keg| keg.version().to_string()).collect();
    assert_eq!(versions, ["1.9.0", "1.10.0"]);
}

#[test]
fn keg_receipt_path_joins_install_receipt_json() {
    let (_temp, root) = utf8_temp();
    let cellar = root.join("Cellar");
    let keg = keg(&cellar, formula_name("ripgrep"), pkg_version("14.1.0"));
    assert_eq!(
        keg.receipt_path(),
        cellar
            .join("ripgrep")
            .join("14.1.0")
            .join("INSTALL_RECEIPT.json")
    );
}

#[test]
fn pin_writes_exact_relative_symlink_target() {
    let (_temp, root) = utf8_temp();
    let pins = root.join("var/homebrew/pinned");
    let cellar = root.join("Cellar");
    let version = pkg_version("1.2.3_1");
    let name = formula_name("foo");
    create_dir(&cellar.join("foo").join("1.2.3_1"));
    let keg = keg(&cellar, name.clone(), version.clone());

    if let Err(err) = pin(&pins, &keg) {
        panic!("pin: {err}");
    }

    let link = pins.join("foo");
    let target = match fs::read_link(link.as_std_path()) {
        Ok(target) => target,
        Err(err) => panic!("read_link: {err}"),
    };
    assert_eq!(target, Path::new("../../../Cellar/foo/1.2.3_1"));
    assert_eq!(pin_relative_target(&keg), "../../../Cellar/foo/1.2.3_1");
    assert!(bool_result(
        is_pinned(&pins, &name, &version),
        "is_pinned pinned version"
    ));
    assert!(!bool_result(
        is_pinned(&pins, &name, &pkg_version("9.9.9")),
        "is_pinned other version"
    ));
}

#[test]
fn pin_rejects_nonexistent_keg_directory() {
    let (_temp, root) = utf8_temp();
    let pins = root.join("var/homebrew/pinned");
    let cellar = root.join("Cellar");
    let keg = keg(&cellar, formula_name("missing"), pkg_version("1.0.0"));

    match pin(&pins, &keg) {
        Err(PrefixError::Io {
            operation,
            path,
            source,
        }) => {
            assert_eq!(operation, "pin");
            assert_eq!(path, keg.path());
            assert_eq!(source.kind(), io::ErrorKind::NotFound);
        }
        Ok(()) => panic!("pin should fail for missing keg directory"),
        Err(err) => panic!("unexpected error: {err}"),
    }
}

#[test]
fn unpin_is_idempotent_and_clears_pin() {
    let (_temp, root) = utf8_temp();
    let pins = root.join("var/homebrew/pinned");
    let cellar = root.join("Cellar");
    let version = pkg_version("2.0.0");
    let name = formula_name("bar");

    if let Err(err) = unpin(&pins, &formula_name("missing")) {
        panic!("unpin missing: {err}");
    }

    create_dir(&cellar.join("bar").join("2.0.0"));
    let keg = keg(&cellar, name.clone(), version.clone());
    if let Err(err) = pin(&pins, &keg) {
        panic!("pin: {err}");
    }
    assert!(bool_result(
        is_pinned(&pins, &name, &version),
        "is_pinned after pin"
    ));

    if let Err(err) = unpin(&pins, &name) {
        panic!("unpin: {err}");
    }
    assert!(!bool_result(
        is_pinned(&pins, &name, &version),
        "is_pinned after unpin"
    ));

    if let Err(err) = unpin(&pins, &name) {
        panic!("second unpin: {err}");
    }
}

#[test]
fn linked_and_opt_resolution_reads_registry_paths() {
    let (_temp, root) = utf8_temp();
    let prefix = root.join("prefix");
    let linked = prefix.join("var/homebrew/linked");
    let cellar_keg = prefix.join("Cellar/git/2.45.0");
    create_dir(&cellar_keg);
    let name = formula_name("git");

    let linked_record = path_result(linked_path(&linked, &name), "linked_path");
    let opt_record = path_result(opt_path(&prefix, &name), "opt_path");

    write_symlink("../../../Cellar/git/2.45.0", &linked_record);
    write_symlink("../Cellar/git/2.45.0", &opt_record);

    let resolved_linked = match resolve_linked(&linked, &name) {
        Ok(Some(path)) => path,
        Ok(None) => panic!("expected linked resolution"),
        Err(err) => panic!("resolve_linked: {err}"),
    };
    let resolved_opt = match resolve_opt(&prefix, &name) {
        Ok(Some(path)) => path,
        Ok(None) => panic!("expected opt resolution"),
        Err(err) => panic!("resolve_opt: {err}"),
    };

    assert_eq!(resolved_linked, linked.join("../../../Cellar/git/2.45.0"));
    assert_eq!(
        resolved_opt,
        prefix.join("opt").join("../Cellar/git/2.45.0")
    );
    assert_eq!(linked_record, linked.join("git"));
    assert_eq!(opt_record, prefix.join("opt/git"));
}

#[test]
fn lock_guard_second_acquire_is_busy_then_succeeds_after_drop() {
    let (_temp, root) = utf8_temp();
    let locks = root.join("var/homebrew/locks");
    let lock_name = "wget.formula.lock";

    let first = match LockGuard::acquire(&locks, lock_name) {
        Ok(guard) => guard,
        Err(err) => panic!("first acquire: {err}"),
    };
    assert_eq!(first.path(), locks.join(lock_name));

    match LockGuard::acquire(&locks, lock_name) {
        Err(PrefixError::LockBusy { command, path }) => {
            assert_eq!(command, "brew");
            assert_eq!(path, locks.join(lock_name));
            let message = PrefixError::LockBusy {
                command: command.clone(),
                path: path.clone(),
            }
            .to_string();
            assert_eq!(
                message,
                format!(
                    "A `brew` process has already locked {}. Please wait for it to finish or terminate it to continue.",
                    locks.join(lock_name)
                )
            );
        }
        Ok(_) => panic!("second acquire should be LockBusy"),
        Err(err) => panic!("unexpected error: {err}"),
    }

    drop(first);

    let second = match LockGuard::acquire(&locks, lock_name) {
        Ok(guard) => guard,
        Err(err) => panic!("post-drop acquire: {err}"),
    };
    drop(second);
}

#[test]
fn prefix_derives_standard_subdirs_from_env() {
    let (_temp, root) = utf8_temp();
    let prefix_root = root.join("hb");
    create_dir(&prefix_root);

    let env = detect_prefix_env(&prefix_root);
    let prefix = Prefix::new(env);
    let wget = formula_name("wget");

    assert_eq!(prefix.path(), prefix_root);
    assert_eq!(prefix.cellar(), prefix_root.join("Cellar"));
    assert_eq!(prefix.locks(), prefix_root.join("var/homebrew/locks"));
    assert_eq!(prefix.pins(), prefix_root.join("var/homebrew/pinned"));
    assert_eq!(prefix.linked(), prefix_root.join("var/homebrew/linked"));
    assert_eq!(prefix.opt(), prefix_root.join("opt"));
    assert_eq!(prefix.bin(), prefix_root.join("bin"));
    assert_eq!(prefix.sbin(), prefix_root.join("sbin"));
    assert_eq!(prefix.etc(), prefix_root.join("etc"));
    assert_eq!(prefix.var(), prefix_root.join("var"));
    assert_eq!(prefix.share(), prefix_root.join("share"));
    assert_eq!(prefix.lib(), prefix_root.join("lib"));
    assert_eq!(prefix.include(), prefix_root.join("include"));
    assert_eq!(
        path_result(prefix.linked_path(&wget), "prefix.linked_path"),
        prefix.linked().join("wget")
    );
    assert_eq!(
        path_result(prefix.opt_path(&wget), "prefix.opt_path"),
        prefix.opt().join("wget")
    );
}
