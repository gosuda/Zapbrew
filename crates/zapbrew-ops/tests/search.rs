use std::collections::HashMap;
use std::io;
use std::sync::{Arc, Mutex};

use camino::Utf8PathBuf;
use serde_json::json;
use tempfile::TempDir;
use zapbrew_api::{CaskCatalog, Catalog};
use zapbrew_ops::search::{self, Args};
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
    let formulae = serde_json::to_vec(&vec![
        json!({
            "name": "retriever",
            "full_name": "retriever",
            "aliases": ["wget", "w-get"],
            "desc": "Retrieve files",
            "versions": {"stable": "1.0", "bottle": false}
        }),
        json!({
            "name": "libfoo",
            "full_name": "libfoo",
            "desc": "Network widget",
            "versions": {"stable": "1.0", "bottle": false}
        }),
        json!({
            "name": "extra",
            "full_name": "extra",
            "desc": "Special Search Phrase",
            "versions": {"stable": "1.0", "bottle": false}
        }),
        json!({
            "name": "special-name",
            "full_name": "special-name",
            "desc": "Ordinary package",
            "versions": {"stable": "1.0", "bottle": false}
        }),
    ])
    .expect("formula payload");
    let casks = serde_json::to_vec(&vec![
        json!({
            "token": "get-app",
            "name": ["Get App"],
            "desc": "Special Search Phrase app",
            "version": "1.0"
        }),
        json!({
            "token": "lib-cask",
            "name": ["Library Cask"],
            "desc": "Visual utility",
            "version": "1.0"
        }),
    ])
    .expect("cask payload");
    let catalog =
        Arc::new(Catalog::from_payload(&formulae, &environment.bottle_tag).expect("catalog"));
    let casks =
        Arc::new(CaskCatalog::from_payload(&casks, &environment.bottle_tag).expect("cask catalog"));
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

fn events(reporter: &RecordingReporter) -> String {
    reporter.take().join("\n---\n")
}

#[tokio::test]
async fn simplified_alias_punctuation_and_regex_sections_are_exact_and_read_only() {
    let temp = TempDir::new().expect("temp");
    let (ctx, reporter) = context(env(&temp));
    let prefix_existed = ctx.env.prefix.exists();

    search::run(
        &ctx,
        Args {
            query: "W-G_ET".to_owned(),
            width: 80,
            ..Args::default()
        },
    )
    .await
    .expect("simplified alias search");
    insta::assert_snapshot!(events(&reporter), @r"
    ohai:Formulae
    ---
    print:retriever
    ");

    search::run(
        &ctx,
        Args {
            query: "/^lib/".to_owned(),
            width: 0,
            ..Args::default()
        },
    )
    .await
    .expect("regex search");
    insta::assert_snapshot!(events(&reporter), @r"
    ohai:Formulae
    ---
    print:libfoo

    ---
    print:
    ---
    ohai:Casks
    ---
    print:lib-cask
    ");
    assert_eq!(
        ctx.env.prefix.exists(),
        prefix_existed,
        "search must not mutate"
    );
}

#[tokio::test]
async fn description_search_is_additive_and_filters_each_catalog() {
    let temp = TempDir::new().expect("temp");
    let (ctx, reporter) = context(env(&temp));

    search::run(
        &ctx,
        Args {
            query: "special".to_owned(),
            desc: true,
            width: 0,
            ..Args::default()
        },
    )
    .await
    .expect("additive description search");
    insta::assert_snapshot!(events(&reporter), @r"
    ohai:Formulae
    ---
    print:extra
    special-name

    ---
    print:
    ---
    ohai:Casks
    ---
    print:get-app
    ");

    search::run(
        &ctx,
        Args {
            query: "special".to_owned(),
            desc: true,
            formula_only: true,
            width: 0,
            ..Args::default()
        },
    )
    .await
    .expect("formula filter");
    insta::assert_snapshot!(events(&reporter), @r"
    ohai:Formulae
    ---
    print:extra
    special-name
    ");

    search::run(
        &ctx,
        Args {
            query: "special".to_owned(),
            desc: true,
            cask_only: true,
            width: 0,
            ..Args::default()
        },
    )
    .await
    .expect("cask filter");
    insta::assert_snapshot!(events(&reporter), @r"
    ohai:Casks
    ---
    print:get-app
    ");
}

#[tokio::test]
async fn invalid_regex_and_empty_results_are_exact_refusals() {
    let temp = TempDir::new().expect("temp");
    let (ctx, reporter) = context(env(&temp));

    let invalid = search::run(
        &ctx,
        Args {
            query: "/+/".to_owned(),
            ..Args::default()
        },
    )
    .await
    .expect_err("invalid regex");
    assert!(matches!(invalid, zapbrew_ops::OpError::Refusal { .. }));
    assert_eq!(invalid.to_string(), "/+/ is not a valid regex.");

    let empty = search::run(
        &ctx,
        Args {
            query: "absent".to_owned(),
            ..Args::default()
        },
    )
    .await
    .expect_err("empty search");
    assert!(matches!(empty, zapbrew_ops::OpError::Refusal { .. }));
    assert_eq!(
        empty.to_string(),
        "No formulae or casks found for \"absent\"."
    );
    assert!(reporter.take().is_empty());
}
