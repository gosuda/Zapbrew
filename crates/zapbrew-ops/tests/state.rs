use std::collections::HashMap;
use std::fmt::Debug;
use std::fs;
use std::str::FromStr;

use camino::Utf8PathBuf;
use tempfile::TempDir;
use zapbrew_ops::OpError;
use zapbrew_ops::state::scan;
use zapbrew_prefix::{
    CommandOutput, CommandRunner, CommandSpec, Env, EnvDetectInput, Keg, RuntimeDependency, Tab,
    pin,
};
use zapbrew_types::{FormulaName, PkgVersion};

fn ok<T, E: Debug>(result: Result<T, E>) -> T {
    match result {
        Ok(value) => value,
        Err(error) => panic!("expected success, got {error:?}"),
    }
}

struct PanicRunner;

impl CommandRunner for PanicRunner {
    fn run(&self, _spec: &CommandSpec) -> Result<CommandOutput, std::io::Error> {
        panic!("Linux scratch Env must not invoke host commands")
    }
}

fn scratch_env() -> (TempDir, Env) {
    let temp = ok(TempDir::new());
    let root = match temp.path().to_str() {
        Some(root) => Utf8PathBuf::from(root),
        None => panic!("temporary path must be UTF-8"),
    };
    let prefix = root.join("prefix");
    let mut vars = HashMap::new();
    vars.insert("HOMEBREW_PREFIX".to_owned(), prefix.to_string());
    vars.insert(
        "HOMEBREW_CELLAR".to_owned(),
        prefix.join("Cellar").to_string(),
    );
    vars.insert("HOMEBREW_CACHE".to_owned(), root.join("cache").to_string());
    vars.insert("HOMEBREW_LOGS".to_owned(), root.join("logs").to_string());
    vars.insert("HOMEBREW_TEMP".to_owned(), root.join("tmp").to_string());
    vars.insert(
        "HOMEBREW_REPOSITORY".to_owned(),
        root.join("repository").to_string(),
    );
    let env = ok(Env::detect_from(
        &EnvDetectInput {
            os: "linux".to_owned(),
            arch: "x86_64".to_owned(),
            home: root.join("home"),
            xdg_cache_home: Some(root.join("xdg-cache")),
            vars,
            available_parallelism: 2,
        },
        &PanicRunner,
    ));
    (temp, env)
}

fn make_keg(env: &Env, name: &str, version: &str, tab: &Tab, payload: &[u8]) -> Keg {
    let formula = ok(FormulaName::from_str(name));
    let version = ok(PkgVersion::from_str(version));
    let keg = ok(Keg::new(&env.cellar, formula, version));
    ok(fs::create_dir_all(keg.path().join("bin")));
    ok(fs::write(keg.path().join("bin/tool"), payload));
    ok(tab.write(keg.receipt_path()));
    keg
}

#[test]
fn missing_cellar_scans_as_an_empty_snapshot() {
    let (_temp, env) = scratch_env();
    let state = ok(scan(&env));
    assert!(state.is_empty());
    assert_eq!(state.len(), 0);
    assert_eq!(state.iter().count(), 0);
}

#[test]
#[cfg(unix)]
fn scan_records_semantic_order_links_pins_files_size_and_receipt_edges() {
    use std::os::unix::fs::symlink;

    let (_temp, env) = scratch_env();
    let dependency_tab = Tab::default();
    let dependent_tab = Tab {
        runtime_dependencies: Some(vec![RuntimeDependency {
            full_name: "dep".to_owned(),
            version: "2.0".to_owned(),
            pkg_version: "2.0".to_owned(),
            declared_directly: true,
            ..RuntimeDependency::default()
        }]),
        ..Tab::default()
    };

    let older = make_keg(&env, "app", "1.2", &dependent_tab, b"old");
    let latest = make_keg(&env, "app", "1.10", &dependent_tab, b"latest-bytes");
    let _dependency = make_keg(&env, "dep", "2.0", &dependency_tab, b"dependency");

    ok(fs::create_dir_all(&env.linked));
    ok(fs::create_dir_all(env.prefix.join("opt")));
    ok(symlink(
        latest.path().as_std_path(),
        env.linked.join("app").as_std_path(),
    ));
    ok(symlink(
        latest.path().as_std_path(),
        env.prefix.join("opt/app").as_std_path(),
    ));
    ok(pin(&env.pins, &older));

    let state = ok(scan(&env));
    assert_eq!(state.len(), 2);
    assert!(state.contains("app"));
    let app = match state.formula("app") {
        Some(app) => app,
        None => panic!("app must be installed"),
    };
    assert_eq!(app.name().name(), "app");
    assert_eq!(
        app.kegs()
            .iter()
            .map(|keg| keg.version().to_string())
            .collect::<Vec<_>>(),
        ["1.2", "1.10"]
    );
    assert_eq!(
        app.latest().map(|keg| keg.version().to_string()),
        Some("1.10".to_owned())
    );
    assert_eq!(
        app.linked().map(|keg| keg.version().to_string()),
        Some("1.10".to_owned())
    );
    assert_eq!(
        app.optlinked().map(|keg| keg.version().to_string()),
        Some("1.10".to_owned())
    );
    assert_eq!(
        app.pinned().map(|keg| keg.version().to_string()),
        Some("1.2".to_owned())
    );

    let latest_state = match app.latest() {
        Some(keg) => keg,
        None => panic!("app must have a latest keg"),
    };
    assert!(latest_state.is_linked());
    assert!(latest_state.is_optlinked());
    assert!(!latest_state.is_pinned());
    assert_eq!(latest_state.file_count(), 2);
    assert_eq!(
        latest_state.files(),
        [
            Utf8PathBuf::from("INSTALL_RECEIPT.json"),
            Utf8PathBuf::from("bin/tool"),
        ]
    );
    assert!(latest_state.size() >= b"latest-bytes".len() as u64);
    assert_eq!(latest_state.path(), latest.path());
    assert_eq!(
        latest_state
            .tab()
            .runtime_dependencies
            .as_ref()
            .map(Vec::len),
        Some(1)
    );

    let dependents = state.dependents_of("homebrew/core/dep");
    assert_eq!(dependents.len(), 1);
    assert_eq!(dependents[0].name().name(), "app");
    assert!(state.dependents_of("missing").is_empty());
}

#[test]
fn malformed_keg_directory_surfaces_the_lower_typed_error() {
    let (_temp, env) = scratch_env();
    ok(fs::create_dir_all(env.cellar.join("bad name/1.0")));

    let error = match scan(&env) {
        Ok(_) => panic!("expected malformed keg failure"),
        Err(error) => error,
    };
    match error {
        OpError::Prefix(_) => {}
        other => panic!("expected prefix error, got {other:?}"),
    }
}
