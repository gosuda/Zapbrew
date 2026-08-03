use std::collections::HashMap;
use std::io;
use std::sync::{Arc, Mutex};

use camino::Utf8PathBuf;
use serde_json::json;
use tempfile::TempDir;
use zapbrew_api::{CaskCatalog, Catalog};
use zapbrew_ops::desc::{self, Args};
use zapbrew_ops::{Ctx, Reporter};
use zapbrew_prefix::{CommandOutput, CommandRunner, CommandSpec, Env, EnvDetectInput};

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

fn env(temp: &TempDir) -> Env {
    let root = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 temp");
    Env::detect_from(
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
    .expect("scratch env")
}

fn context(environment: Env) -> (Ctx, Arc<RecordingReporter>) {
    let payload = serde_json::to_vec(&vec![
        json!({
            "name": "alpha",
            "full_name": "homebrew/core/alpha",
            "aliases": ["a"],
            "oldnames": ["first"],
            "desc": "Alpha description",
            "versions": {"stable": "1.0", "bottle": false}
        }),
        json!({
            "name": "beta",
            "full_name": "homebrew/core/beta",
            "desc": null,
            "versions": {"stable": "1.0", "bottle": false}
        }),
        json!({
            "name": "gamma",
            "full_name": "other/tools/gamma",
            "desc": "Gamma description",
            "versions": {"stable": "1.0", "bottle": false}
        }),
    ])
    .expect("formula payload");
    let catalog =
        Arc::new(Catalog::from_payload(&payload, &environment.bottle_tag).expect("catalog"));
    let casks =
        Arc::new(CaskCatalog::from_payload(b"[]", &environment.bottle_tag).expect("cask catalog"));
    let recording = Arc::new(RecordingReporter::default());
    let reporter: Arc<dyn Reporter> = recording.clone();
    (
        Ctx {
            env: environment,
            http: reqwest::Client::new(),
            catalog,
            casks,
            commands: Arc::new(PanicRunner),
            reporter,
        },
        recording,
    )
}

#[tokio::test]
async fn hit_alias_oldname_null_and_argument_order_are_exact_and_read_only() {
    let temp = TempDir::new().expect("temp");
    let (ctx, reporter) = context(env(&temp));
    let prefix_existed = ctx.env.prefix.exists();

    desc::run(
        &ctx,
        Args {
            names: vec![
                "gamma".to_owned(),
                "a".to_owned(),
                "beta".to_owned(),
                "first".to_owned(),
            ],
        },
    )
    .await
    .expect("descriptions");

    insta::assert_snapshot!(reporter.take().join("\n"), @r"
    print:other/tools/gamma: Gamma description
    print:homebrew/core/alpha: Alpha description
    print:homebrew/core/alpha: Alpha description
    ");
    assert_eq!(
        ctx.env.prefix.exists(),
        prefix_existed,
        "desc must not mutate"
    );
}

#[tokio::test]
async fn missing_formula_is_typed_and_emits_nothing() {
    let temp = TempDir::new().expect("temp");
    let (ctx, reporter) = context(env(&temp));
    let error = desc::run(
        &ctx,
        Args {
            names: vec!["missing".to_owned()],
        },
    )
    .await
    .expect_err("missing formula");

    assert!(matches!(
        error,
        zapbrew_ops::OpError::MissingFormula { ref name } if name == "missing"
    ));
    assert_eq!(
        error.to_string(),
        "No available formula with the name \"missing\"."
    );
    assert!(reporter.take().is_empty());
}
