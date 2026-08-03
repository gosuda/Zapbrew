use std::collections::HashMap;
use std::io;
use std::str::FromStr;
use std::sync::{Arc, Mutex};

use camino::Utf8PathBuf;
use serde_json::{Value, json};
use tempfile::TempDir;
use zapbrew_api::{CaskCatalog, Catalog};
use zapbrew_ops::outdated::{self, Args};
use zapbrew_ops::{Ctx, Reporter};
use zapbrew_prefix::{
    CommandOutput, CommandRunner, CommandSpec, Env, EnvDetectInput, Keg, Source, SourceVersions,
    Tab, pin,
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

fn context(env: Env, formulae: Vec<Value>) -> (Ctx, Arc<RecordingReporter>) {
    let payload = serde_json::to_vec(&formulae).expect("catalog payload");
    let catalog = Arc::new(Catalog::from_payload(&payload, &env.bottle_tag).expect("catalog"));
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

fn formula(name: &str, version: &str, revision: u32, scheme: u32) -> Value {
    json!({
        "name": name,
        "full_name": name,
        "versions": {"stable": version, "bottle": true},
        "revision": revision,
        "version_scheme": scheme
    })
}

fn keg(env: &Env, name: &str, version: &str, scheme: u32) -> Keg {
    let keg = Keg::new(
        &env.cellar,
        FormulaName::from_str(name).expect("formula name"),
        PkgVersion::from_str(version).expect("pkg version"),
    )
    .expect("keg");
    std::fs::create_dir_all(keg.path()).expect("keg dir");
    Tab {
        installed_on_request: true,
        source: Source {
            versions: SourceVersions {
                stable: Some(keg.version().version.to_string()),
                version_scheme: scheme,
                ..SourceVersions::default()
            },
            ..Source::default()
        },
        ..Tab::default()
    }
    .write(keg.receipt_path())
    .expect("tab");
    keg
}

#[tokio::test]
async fn predicate_matrix_uses_semantic_pkg_versions_and_scheme_rule() {
    let temp = TempDir::new().expect("temp");
    let env = env(&temp);
    let formulae = vec![
        formula("higher", "2.0", 0, 0),
        formula("equal", "1.0", 0, 0),
        formula("lower", "1.0", 0, 0),
        formula("revision", "1.0", 1, 0),
        formula("scheme-different", "1.0", 0, 1),
        formula("scheme-equal", "1.0", 0, 1),
    ];
    keg(&env, "higher", "1.0", 0);
    keg(&env, "equal", "1.0", 0);
    keg(&env, "lower", "2.0", 0);
    keg(&env, "revision", "1.0", 0);
    keg(&env, "scheme-different", "9.0", 0);
    keg(&env, "scheme-equal", "1.0", 0);
    let (ctx, reporter) = context(env, formulae);

    outdated::run(&ctx, Args::default())
        .await
        .expect("outdated");

    assert_eq!(
        reporter.take(),
        vec![
            "print:higher".to_owned(),
            "print:revision".to_owned(),
            "print:scheme-different".to_owned(),
        ]
    );
}

#[tokio::test]
async fn formula_is_outdated_only_when_every_installed_keg_is_outdated() {
    let temp = TempDir::new().expect("temp");
    let env = env(&temp);
    let formulae = vec![
        formula("all-old", "2.0", 0, 1),
        formula("empty", "2.0", 0, 0),
        formula("mixed-scheme", "1.0", 0, 1),
        formula("mixed-version", "2.0", 0, 0),
        formula("revision-mix", "1.0", 1, 0),
    ];
    keg(&env, "all-old", "1.0", 1);
    keg(&env, "all-old", "9.0", 0);
    std::fs::create_dir_all(env.cellar.join("empty")).expect("empty rack");
    keg(&env, "mixed-scheme", "9.0", 0);
    keg(&env, "mixed-scheme", "1.0", 0);
    keg(&env, "mixed-version", "1.0", 0);
    keg(&env, "mixed-version", "2.0", 0);
    keg(&env, "revision-mix", "1.0", 0);
    let (ctx, reporter) = context(env, formulae);

    outdated::run(&ctx, Args::default())
        .await
        .expect("outdated");
    assert_eq!(
        reporter.take(),
        vec!["print:all-old".to_owned(), "print:revision-mix".to_owned()]
    );

    let error = outdated::run(
        &ctx,
        Args {
            names: vec!["empty".to_owned()],
            ..Args::default()
        },
    )
    .await
    .expect_err("empty rack is not installed");
    assert_eq!(error.to_string(), "empty is not installed");
}

#[tokio::test]
async fn default_verbose_pinned_and_json_v2_are_exact() {
    let temp = TempDir::new().expect("temp");
    let env = env(&temp);
    let installed = keg(&env, "foo", "1.0", 0);
    pin(&env.pins, &installed).expect("pin");
    let (ctx, reporter) = context(env, vec![formula("foo", "1.1", 0, 0)]);

    outdated::run(&ctx, Args::default()).await.expect("default");
    assert_eq!(reporter.take(), vec!["print:foo"]);

    outdated::run(
        &ctx,
        Args {
            verbose: true,
            ..Args::default()
        },
    )
    .await
    .expect("verbose");
    assert_eq!(
        reporter.take(),
        vec!["print:foo (1.0) < 1.1 [pinned at 1.0]"]
    );

    outdated::run(
        &ctx,
        Args {
            json_v2: true,
            ..Args::default()
        },
    )
    .await
    .expect("json");
    assert_eq!(
        reporter.take(),
        vec![concat!(
            "print:{\n",
            "  \"formulae\": [\n",
            "    {\n",
            "      \"name\": \"foo\",\n",
            "      \"installed_versions\": [\"1.0\"],\n",
            "      \"current_version\": \"1.1\",\n",
            "      \"pinned\": true,\n",
            "      \"pinned_version\": \"1.0\"\n",
            "    }\n",
            "  ],\n",
            "  \"casks\": []\n",
            "}"
        )]
    );
}

#[tokio::test]
async fn json_v2_uses_null_for_unpinned_formula() {
    let temp = TempDir::new().expect("temp");
    let env = env(&temp);
    keg(&env, "foo", "1.0", 0);
    let (ctx, reporter) = context(env, vec![formula("foo", "1.1", 0, 0)]);

    outdated::run(
        &ctx,
        Args {
            json_v2: true,
            ..Args::default()
        },
    )
    .await
    .expect("json");
    let output = reporter.take().join("\n");
    assert!(output.contains("\"pinned\": false"));
    assert!(output.contains("\"pinned_version\": null"));
    assert!(output.contains("\"casks\": []"));
}

#[tokio::test]
async fn named_mode_refuses_not_installed_and_warns_for_missing_api() {
    let temp = TempDir::new().expect("temp");
    let env = env(&temp);
    keg(&env, "ghost", "1.0", 0);
    let (ctx, reporter) = context(env, vec![formula("known", "2.0", 0, 0)]);

    let error = outdated::run(
        &ctx,
        Args {
            names: vec!["known".to_owned()],
            ..Args::default()
        },
    )
    .await
    .expect_err("not installed");
    assert_eq!(error.to_string(), "known is not installed");

    outdated::run(
        &ctx,
        Args {
            names: vec!["ghost".to_owned()],
            ..Args::default()
        },
    )
    .await
    .expect("missing API skipped");
    assert_eq!(
        reporter.take(),
        vec!["opoo:ghost is installed but unavailable in the formula API; skipping."]
    );
}
