use std::collections::HashMap;
use std::io;
use std::os::unix::fs::symlink;
use std::str::FromStr;
use std::sync::{Arc, Mutex};

use camino::Utf8PathBuf;
use tempfile::TempDir;
use zapbrew_api::{CaskCatalog, Catalog};
use zapbrew_ops::leaves::{self, Args, Filter};
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
    fn take(&self) -> Vec<String> {
        std::mem::take(&mut *self.0.lock().expect("reporter lock"))
    }
}
impl Reporter for RecordingReporter {
    fn ohai(&self, _: &str) {}
    fn oh1(&self, _: &str) {}
    fn opoo(&self, _: &str) {}
    fn onoe(&self, _: &str) {}
    fn print(&self, message: &str) {
        self.0
            .lock()
            .expect("reporter lock")
            .push(message.to_owned());
    }
    fn eprint(&self, _: &str) {}
}

fn context() -> (TempDir, Ctx, Arc<RecordingReporter>) {
    let temp = TempDir::new().expect("temp");
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
    let ctx = Ctx {
        env,
        http: reqwest::Client::new(),
        catalog,
        casks,
        commands: Arc::new(PanicRunner),
        reporter,
    };
    (temp, ctx, recording)
}

fn keg(env: &Env, name: &str, version: &str, requested: bool, dependencies: &[&str]) -> Keg {
    let keg = Keg::new(
        &env.cellar,
        FormulaName::from_str(name).expect("formula name"),
        PkgVersion::from_str(version).expect("version"),
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

fn link(env: &Env, name: &str, keg: &Keg) {
    std::fs::create_dir_all(&env.linked).expect("linked dir");
    symlink(keg.path(), env.linked.join(name)).expect("linked keg");
}

fn optlink(env: &Env, name: &str, keg: &Keg) {
    std::fs::create_dir_all(env.prefix.join("opt")).expect("opt dir");
    symlink(keg.path(), env.prefix.join("opt").join(name)).expect("opt keg");
}

#[tokio::test]
async fn leaves_match_tap_prefixed_reverse_dependencies_and_active_keg_precedence() {
    let (_temp, ctx, reporter) = context();
    keg(&ctx.env, "leaf-a", "1.0", false, &[]);
    keg(&ctx.env, "blocker", "1.0", true, &["homebrew/core/leaf-a"]);

    let opt_old = keg(&ctx.env, "opt-choice", "1.0", true, &[]);
    let opt_new = keg(&ctx.env, "opt-choice", "2.0", false, &[]);
    optlink(&ctx.env, "opt-choice", &opt_old);
    link(&ctx.env, "opt-choice", &opt_new);

    let linked_old = keg(&ctx.env, "linked-choice", "1.0", false, &[]);
    keg(&ctx.env, "linked-choice", "2.0", true, &[]);
    link(&ctx.env, "linked-choice", &linked_old);

    keg(&ctx.env, "sole-choice", "1.0", true, &[]);
    keg(&ctx.env, "latest-choice", "1.0", true, &[]);
    keg(&ctx.env, "latest-choice", "2.0", false, &[]);

    leaves::run(&ctx, Args::default())
        .await
        .expect("all leaves");
    assert_eq!(
        reporter.take(),
        [
            "blocker",
            "latest-choice",
            "linked-choice",
            "opt-choice",
            "sole-choice"
        ]
    );

    leaves::run(
        &ctx,
        Args {
            filter: Filter::OnRequest,
        },
    )
    .await
    .expect("requested leaves");
    assert_eq!(reporter.take(), ["blocker", "opt-choice", "sole-choice"]);

    leaves::run(
        &ctx,
        Args {
            filter: Filter::AsDependency,
        },
    )
    .await
    .expect("dependency leaves");
    assert_eq!(reporter.take(), ["latest-choice", "linked-choice"]);
}

#[tokio::test]
async fn empty_leaf_set_emits_nothing() {
    let (_temp, ctx, reporter) = context();
    keg(&ctx.env, "a", "1.0", false, &["b"]);
    keg(&ctx.env, "b", "1.0", false, &["a"]);

    leaves::run(&ctx, Args::default()).await.expect("no leaves");
    assert!(reporter.take().is_empty());
}
