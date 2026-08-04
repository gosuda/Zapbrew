use std::collections::HashMap;
use std::io;
use std::str::FromStr;
use std::sync::{Arc, Mutex};

use camino::Utf8PathBuf;
use tempfile::TempDir;
use zapbrew_api::{CaskCatalog, Catalog};
use zapbrew_ops::transaction_test_support::{fail_next_backup_cleanup, fail_removal_after};
use zapbrew_ops::uninstall::{self, Args};
use zapbrew_ops::{Ctx, OpError, Reporter};
use zapbrew_pour::LinkOptions;
use zapbrew_prefix::{
    CommandOutput, CommandRunner, CommandSpec, Env, EnvDetectInput, Keg, Prefix, RuntimeDependency,
    Source, SourceVersions, Tab, pin,
};
use zapbrew_types::{FormulaName, PkgVersion};

struct PanicRunner;
impl CommandRunner for PanicRunner {
    fn run(&self, spec: &CommandSpec) -> Result<CommandOutput, io::Error> {
        panic!("host command must not run: {:?}", spec.program())
    }
}

#[derive(Default)]
struct RecordingReporter(Mutex<Vec<String>>);
impl RecordingReporter {
    fn push(&self, channel: &str, message: &str) {
        self.0
            .lock()
            .expect("reporter lock")
            .push(format!("{channel}:{message}"));
    }
    fn take(&self) -> Vec<String> {
        std::mem::take(&mut *self.0.lock().expect("reporter lock"))
    }
}
impl Reporter for RecordingReporter {
    fn ohai(&self, message: &str) {
        self.push("ohai", message);
    }
    fn oh1(&self, message: &str) {
        self.push("oh1", message);
    }
    fn opoo(&self, message: &str) {
        self.push("opoo", message);
    }
    fn onoe(&self, message: &str) {
        self.push("onoe", message);
    }
    fn print(&self, message: &str) {
        self.push("print", message);
    }
    fn eprint(&self, message: &str) {
        self.push("eprint", message);
    }
}

fn env(temp: &TempDir, no_autoremove: bool) -> Env {
    let root = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 temp");
    let mut vars = HashMap::from([
        (
            "HOMEBREW_PREFIX".to_owned(),
            root.join("prefix").to_string(),
        ),
        ("HOMEBREW_CACHE".to_owned(), root.join("cache").to_string()),
        ("HOMEBREW_TEMP".to_owned(), root.join("temp").to_string()),
    ]);
    if no_autoremove {
        vars.insert("HOMEBREW_NO_AUTOREMOVE".to_owned(), "1".to_owned());
    }
    Env::detect_from(
        &EnvDetectInput {
            os: "linux".to_owned(),
            arch: "x86_64".to_owned(),
            home: root.join("home"),
            xdg_cache_home: None,
            vars,
            available_parallelism: 2,
        },
        &PanicRunner,
    )
    .expect("scratch env")
}

fn context(env: Env) -> (Ctx, Arc<RecordingReporter>) {
    let catalog = Arc::new(Catalog::from_payload(b"[]", &env.bottle_tag).expect("catalog"));
    let casks = Arc::new(CaskCatalog::from_payload(b"[]", &env.bottle_tag).expect("casks"));
    let recording = Arc::new(RecordingReporter::default());
    let reporter: Arc<dyn Reporter> = recording.clone();
    (
        Ctx {
            env,
            http: reqwest::Client::new(),
            catalog,
            casks,
            commands: Arc::new(PanicRunner),
            reporter,
        },
        recording,
    )
}

fn keg(
    env: &Env,
    name: &str,
    version: &str,
    requested: bool,
    dependencies: &[&str],
    linked: bool,
) -> Keg {
    let name = FormulaName::from_str(name).expect("formula name");
    let version = PkgVersion::from_str(version).expect("pkg version");
    let keg = Keg::new(&env.cellar, name, version).expect("keg");
    std::fs::create_dir_all(keg.path().join("bin")).expect("keg dirs");
    std::fs::write(keg.path().join("bin/tool"), b"tool").expect("keg file");
    Tab {
        installed_on_request: requested,
        runtime_dependencies: Some(
            dependencies
                .iter()
                .map(|dependency| RuntimeDependency {
                    full_name: (*dependency).to_owned(),
                    version: "1.0".to_owned(),
                    revision: 0,
                    bottle_rebuild: None,
                    pkg_version: "1.0".to_owned(),
                    declared_directly: true,
                    compatibility_version: None,
                })
                .collect(),
        ),
        source: Source {
            versions: SourceVersions {
                stable: Some(keg.version().to_string()),
                version_scheme: 0,
                ..SourceVersions::default()
            },
            ..Source::default()
        },
        ..Tab::default()
    }
    .write(keg.receipt_path())
    .expect("tab");
    if linked {
        zapbrew_pour::link(&keg, &Prefix::new(env.clone()), LinkOptions::default())
            .expect("link keg");
    }
    keg
}

fn args(names: &[&str]) -> Args {
    Args {
        names: names.iter().map(|name| (*name).to_owned()).collect(),
        ..Args::default()
    }
}

#[tokio::test]
async fn uninstalls_formula_absent_from_catalog_and_cleans_records() {
    let temp = TempDir::new().expect("temp");
    let (ctx, reporter) = context(env(&temp, true));
    let old = keg(&ctx.env, "foo", "1.0", true, &[], true);

    uninstall::run(&ctx, args(&["foo"]))
        .await
        .expect("uninstall");

    assert!(!old.path().exists());
    assert!(!ctx.env.cellar.join("foo").exists());
    assert!(!ctx.env.prefix.join("opt/foo").exists());
    assert!(!ctx.env.linked.join("foo").exists());
    assert_eq!(
        reporter.take(),
        vec![format!("print:Uninstalling {}... (643B)", old.path())]
    );
}

#[tokio::test]
async fn refuses_installed_dependents_and_ignore_dependencies_bypasses() {
    let temp = TempDir::new().expect("temp");
    let (ctx, _) = context(env(&temp, true));
    let dependency = keg(&ctx.env, "dep", "1.0", false, &[], true);
    let _app = keg(&ctx.env, "app", "1.0", true, &["dep"], false);

    let error = uninstall::run(&ctx, args(&["dep"]))
        .await
        .expect_err("dependent refusal");
    assert_eq!(
        error.to_string(),
        format!(
            "Refusing to uninstall {}\nbecause it is required by app, which is currently installed.\nYou can override this and force removal with:\n  brew uninstall --ignore-dependencies dep",
            dependency.path()
        )
    );
    assert!(dependency.path().exists());

    uninstall::run(
        &ctx,
        Args {
            ignore_dependencies: true,
            ..args(&["dep"])
        },
    )
    .await
    .expect("ignore dependency removal");
    assert!(!dependency.path().exists());
}

#[tokio::test]
async fn pinned_formula_is_untouched() {
    let temp = TempDir::new().expect("temp");
    let (ctx, _) = context(env(&temp, true));
    let installed = keg(&ctx.env, "foo", "1.0", true, &[], true);
    pin(&ctx.env.pins, &installed).expect("pin");

    let error = uninstall::run(&ctx, args(&["foo"]))
        .await
        .expect_err("pinned refusal");
    assert_eq!(
        error.to_string(),
        "foo is pinned. You must unpin it to uninstall."
    );
    assert!(installed.path().exists());
    assert!(ctx.env.pins.join("foo").exists());
}

#[tokio::test]
async fn non_force_removes_active_keg_and_force_removes_every_version() {
    let temp = TempDir::new().expect("temp");
    let (ctx, reporter) = context(env(&temp, true));
    let first = keg(&ctx.env, "foo", "1.0", true, &[], true);
    let second = keg(&ctx.env, "foo", "2.0", true, &[], false);

    uninstall::run(&ctx, args(&["foo"]))
        .await
        .expect("remove active");
    assert!(!first.path().exists());
    assert!(second.path().exists());
    assert_eq!(
        reporter.take(),
        vec![
            format!("print:Uninstalling {}... (643B)", first.path()),
            "print:foo 2.0 is still installed.\nTo remove all versions, run:\n  brew uninstall --force foo".to_owned(),
        ]
    );

    uninstall::run(
        &ctx,
        Args {
            force: true,
            ..args(&["foo"])
        },
    )
    .await
    .expect("force remove");
    assert!(!second.path().exists());
    assert!(!ctx.env.cellar.join("foo").exists());
}

#[tokio::test]
async fn reports_leftover_configuration_paths_exactly() {
    let temp = TempDir::new().expect("temp");
    let (ctx, reporter) = context(env(&temp, true));
    let installed = keg(&ctx.env, "foo", "1.0", true, &[], false);
    let config = ctx.env.prefix.join("etc/foo/config.toml");
    std::fs::create_dir_all(config.parent().expect("config parent")).expect("config dir");
    std::fs::write(&config, b"x").expect("config");

    uninstall::run(&ctx, args(&["foo"]))
        .await
        .expect("uninstall");
    assert_eq!(
        reporter.take(),
        vec![
            format!("print:Uninstalling {}... (643B)", installed.path()),
            format!(
                "opoo:The following foo configuration files have not been removed!\nIf desired, remove them manually with rm -rf:\n  {config}"
            ),
        ]
    );
}

#[tokio::test]
async fn precommit_failure_restores_normal_and_keg_only_link_surfaces() {
    let temp = TempDir::new().expect("temp");
    let (ctx, _) = context(env(&temp, true));
    let installed = keg(&ctx.env, "rollbackfoo", "1.0", true, &[], true);
    let linked = ctx.env.linked.join("rollbackfoo");
    let opt = ctx.env.prefix.join("opt/rollbackfoo");
    fail_removal_after("rollbackfoo", 1).expect("arm failure");

    let error = uninstall::run(&ctx, args(&["rollbackfoo"]))
        .await
        .expect_err("injected failure");
    assert!(
        error
            .to_string()
            .contains("injected pre-commit removal failure")
    );
    assert!(installed.path().exists());
    assert_eq!(
        std::fs::canonicalize(&linked).expect("linked target"),
        std::fs::canonicalize(installed.path()).expect("keg target")
    );
    assert_eq!(
        std::fs::canonicalize(&opt).expect("opt target"),
        std::fs::canonicalize(installed.path()).expect("keg target")
    );
    assert!(ctx.env.prefix.join("bin/tool").exists());

    let keg_only_temp = TempDir::new().expect("temp");
    let (keg_only_ctx, _) = context(env(&keg_only_temp, true));
    let keg_only = keg(
        &keg_only_ctx.env,
        "kegonlyrollback",
        "1.0",
        true,
        &[],
        false,
    );
    zapbrew_pour::link(
        &keg_only,
        &Prefix::new(keg_only_ctx.env.clone()),
        LinkOptions {
            keg_only: true,
            ..LinkOptions::default()
        },
    )
    .expect("keg-only link");
    let prefix_file = keg_only_ctx.env.prefix.join("bin/tool");
    assert!(!prefix_file.exists());
    fail_removal_after("kegonlyrollback", 1).expect("arm failure");

    uninstall::run(&keg_only_ctx, args(&["kegonlyrollback"]))
        .await
        .expect_err("injected failure");

    assert!(keg_only.path().exists());
    assert!(keg_only_ctx.env.linked.join("kegonlyrollback").exists());
    assert!(!prefix_file.exists());
}

#[tokio::test]
async fn postcommit_cleanup_failure_keeps_formula_removed_and_reports_trash() {
    let temp = TempDir::new().expect("temp");
    let (ctx, _) = context(env(&temp, true));
    let installed = keg(&ctx.env, "cleanupfoo", "1.0", true, &[], true);
    fail_next_backup_cleanup("cleanupfoo").expect("arm cleanup failure");

    let error = uninstall::run(&ctx, args(&["cleanupfoo"]))
        .await
        .expect_err("cleanup incomplete");
    assert!(matches!(error, OpError::CleanupIncomplete { .. }));
    assert!(!installed.path().exists());
    assert!(!ctx.env.cellar.join("cleanupfoo").exists());
    assert!(!ctx.env.linked.join("cleanupfoo").exists());
    assert!(error.to_string().contains("trash"));
}

#[tokio::test]
async fn uninstall_hands_off_to_autoremove_unless_suppressed() {
    let temp = TempDir::new().expect("temp");
    let (ctx, _) = context(env(&temp, false));
    let dependency = keg(&ctx.env, "dep", "1.0", false, &[], false);
    let root = keg(&ctx.env, "root", "1.0", true, &["dep"], false);

    uninstall::run(&ctx, args(&["root"]))
        .await
        .expect("uninstall");
    assert!(!root.path().exists());
    assert!(!dependency.path().exists());

    let temp = TempDir::new().expect("temp");
    let (ctx, _) = context(env(&temp, true));
    let dependency = keg(&ctx.env, "dep", "1.0", false, &[], false);
    let _root = keg(&ctx.env, "root", "1.0", true, &["dep"], false);
    uninstall::run(&ctx, args(&["root"]))
        .await
        .expect("uninstall");
    assert!(dependency.path().exists());
}

#[tokio::test]
async fn not_installed_refusal_names_the_cellar_keg() {
    let temp = TempDir::new().expect("temp");
    let (ctx, _) = context(env(&temp, true));
    let error = uninstall::run(&ctx, args(&["missing"]))
        .await
        .expect_err("not installed");
    assert_eq!(
        error.to_string(),
        format!("No such keg: {}/missing", ctx.env.cellar)
    );
}
