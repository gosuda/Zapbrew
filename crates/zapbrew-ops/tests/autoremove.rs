use std::collections::HashMap;
use std::io;
use std::str::FromStr;
use std::sync::{Arc, Mutex};

use camino::Utf8PathBuf;
use tempfile::TempDir;
use zapbrew_api::{CaskCatalog, Catalog};
use zapbrew_ops::autoremove::{self, Args};
use zapbrew_ops::{Ctx, Reporter};
use zapbrew_prefix::{
    CommandOutput, CommandRunner, CommandSpec, Env, EnvDetectInput, Keg, RuntimeDependency, Tab,
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

fn context(temp: &TempDir) -> (Ctx, Arc<RecordingReporter>) {
    let root = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 temp");
    let env = Env::detect_from(
        &EnvDetectInput {
            os: "linux".to_owned(),
            arch: "x86_64".to_owned(),
            home: root.join("home"),
            xdg_cache_home: None,
            vars: HashMap::from([
                (
                    "HOMEBREW_PREFIX".to_owned(),
                    root.join("prefix").to_string(),
                ),
                ("HOMEBREW_CACHE".to_owned(), root.join("cache").to_string()),
                ("HOMEBREW_TEMP".to_owned(), root.join("temp").to_string()),
            ]),
            available_parallelism: 2,
        },
        &PanicRunner,
    )
    .expect("scratch env");
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

fn keg(env: &Env, name: &str, requested: bool, dependencies: &[&str]) -> Keg {
    let keg = Keg::new(
        &env.cellar,
        FormulaName::from_str(name).expect("formula name"),
        PkgVersion::from_str("1.0").expect("version"),
    )
    .expect("keg");
    std::fs::create_dir_all(keg.path()).expect("keg dir");
    Tab {
        installed_on_request: requested,
        runtime_dependencies: Some(
            dependencies
                .iter()
                .map(|name| RuntimeDependency {
                    full_name: (*name).to_owned(),
                    version: "1.0".to_owned(),
                    revision: 0,
                    bottle_rebuild: None,
                    pkg_version: "1.0".to_owned(),
                    declared_directly: true,
                    compatibility_version: None,
                })
                .collect(),
        ),
        ..Tab::default()
    }
    .write(keg.receipt_path())
    .expect("tab");
    keg
}

#[tokio::test]
async fn removes_the_complete_multi_round_fixpoint() {
    let temp = TempDir::new().expect("temp");
    let (ctx, reporter) = context(&temp);
    let root = keg(&ctx.env, "root", false, &["middle"]);
    let middle = keg(&ctx.env, "middle", false, &["leaf"]);
    let leaf = keg(&ctx.env, "leaf", false, &[]);

    autoremove::run(&ctx, Args::default())
        .await
        .expect("autoremove");

    assert!(!root.path().exists());
    assert!(!middle.path().exists());
    assert!(!leaf.path().exists());
    let messages = reporter.take();
    assert_eq!(messages[0], "oh1:Autoremoving 3 unneeded formulae:");
    assert_eq!(messages[1], "print:leaf\nmiddle\nroot");
}

#[tokio::test]
async fn dry_run_prints_exact_sorted_plan_without_mutation() {
    let temp = TempDir::new().expect("temp");
    let (ctx, reporter) = context(&temp);
    let beta = keg(&ctx.env, "beta", false, &[]);
    let alpha = keg(&ctx.env, "alpha", false, &[]);

    autoremove::run(&ctx, Args { dry_run: true })
        .await
        .expect("dry run");

    assert!(alpha.path().exists());
    assert!(beta.path().exists());
    assert_eq!(
        reporter.take(),
        vec![
            "oh1:Would autoremove 2 unneeded formulae:".to_owned(),
            "print:alpha\nbeta".to_owned(),
        ]
    );
}

#[tokio::test]
async fn single_candidate_dry_run_uses_fixed_formulae_noun() {
    let temp = TempDir::new().expect("temp");
    let (ctx, reporter) = context(&temp);
    let candidate = keg(&ctx.env, "solo", false, &[]);

    autoremove::run(&ctx, Args { dry_run: true })
        .await
        .expect("dry run");

    assert!(candidate.path().exists());
    assert_eq!(
        reporter.take(),
        vec![
            "oh1:Would autoremove 1 unneeded formulae:".to_owned(),
            "print:solo".to_owned(),
        ]
    );
}

#[tokio::test]
async fn requested_formula_and_its_dependency_are_not_candidates() {
    let temp = TempDir::new().expect("temp");
    let (ctx, reporter) = context(&temp);
    let app = keg(&ctx.env, "app", true, &["dep"]);
    let dep = keg(&ctx.env, "dep", false, &[]);

    autoremove::run(&ctx, Args::default())
        .await
        .expect("autoremove");

    assert!(app.path().exists());
    assert!(dep.path().exists());
    assert!(reporter.take().is_empty());
}

#[tokio::test]
async fn missing_or_corrupt_receipts_are_typed_errors() {
    let temp = TempDir::new().expect("temp");
    let (ctx, _) = context(&temp);
    let missing = Keg::new(
        &ctx.env.cellar,
        FormulaName::from_str("missingtab").expect("name"),
        PkgVersion::from_str("1.0").expect("version"),
    )
    .expect("keg");
    std::fs::create_dir_all(missing.path()).expect("keg dir");
    let error = autoremove::run(&ctx, Args::default())
        .await
        .expect_err("missing tab");
    assert!(error.to_string().contains("read autoremove receipt"));

    std::fs::write(missing.receipt_path(), b"not json").expect("corrupt tab");
    let error = autoremove::run(&ctx, Args::default())
        .await
        .expect_err("corrupt tab");
    assert!(error.to_string().contains("invalid autoremove receipt"));
}
