use std::collections::HashMap;
use std::io;
use std::str::FromStr;
use std::sync::{Arc, Mutex};

use camino::Utf8PathBuf;
use tempfile::TempDir;
use zapbrew_api::{CaskCatalog, Catalog};
use zapbrew_ops::uses::{self, Args};
use zapbrew_ops::{Ctx, OpError, Reporter};
use zapbrew_prefix::{CommandOutput, CommandRunner, CommandSpec, Env, EnvDetectInput, Keg, Tab};
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

fn context(payload: &[u8]) -> (TempDir, Ctx, Arc<RecordingReporter>) {
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
    let catalog = Arc::new(Catalog::from_payload(payload, &env.bottle_tag).expect("catalog"));
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

fn args(names: &[&str]) -> Args {
    Args {
        names: names.iter().map(|name| (*name).to_owned()).collect(),
        width: 0,
        ..Args::default()
    }
}

fn install(env: &Env, name: &str) {
    let keg = Keg::new(
        &env.cellar,
        FormulaName::from_str(name).expect("formula name"),
        PkgVersion::from_str("1.0").expect("version"),
    )
    .expect("keg");
    std::fs::create_dir_all(keg.path()).expect("keg dir");
    Tab::default().write(keg.receipt_path()).expect("tab");
}

const GRAPH: &[u8] = br#"[
  {"name":"base","full_name":"base","versions":{"stable":"1"}},
  {"name":"other","full_name":"other","versions":{"stable":"1"}},
  {"name":"alpha","full_name":"alpha","versions":{"stable":"1"},"dependencies":["base","other"]},
  {"name":"beta","full_name":"beta","versions":{"stable":"1"},"dependencies":["other"]},
  {"name":"zeta","full_name":"zeta","versions":{"stable":"1"},"recommended_dependencies":["base"]},
  {"name":"top","full_name":"top","versions":{"stable":"1"},"dependencies":["zeta"]},
  {"name":"optional-user","full_name":"optional-user","versions":{"stable":"1"},"optional_dependencies":["base"]},
  {"name":"build-user","full_name":"build-user","versions":{"stable":"1"},"build_dependencies":["base"]},
  {"name":"test-user","full_name":"test-user","versions":{"stable":"1"},"test_dependencies":["base"]},
  {"name":"recommended-build-user","full_name":"recommended-build-user","versions":{"stable":"1"},
   "uses_from_macos":[{"base":["recommended","build"]}]}
]"#;

#[tokio::test]
async fn direct_recursive_intersection_and_width_are_deterministic() {
    let (_temp, ctx, reporter) = context(GRAPH);
    uses::run(&ctx, args(&["base"])).await.expect("direct");
    assert_eq!(reporter.take(), ["alpha\nrecommended-build-user\nzeta\n"]);

    uses::run(
        &ctx,
        Args {
            width: 20,
            ..args(&["other"])
        },
    )
    .await
    .expect("columns");
    assert_eq!(reporter.take(), ["alpha      beta\n"]);

    uses::run(
        &ctx,
        Args {
            recursive: true,
            ..args(&["base"])
        },
    )
    .await
    .expect("recursive");
    assert_eq!(
        reporter.take(),
        ["alpha\nrecommended-build-user\ntop\nzeta\n"]
    );

    uses::run(&ctx, args(&["base", "other"]))
        .await
        .expect("intersection");
    assert_eq!(reporter.take(), ["alpha\n"]);
}

#[tokio::test]
async fn installed_and_tag_filters_select_candidates_without_mutation() {
    let (_temp, ctx, reporter) = context(GRAPH);
    install(&ctx.env, "zeta");
    uses::run(
        &ctx,
        Args {
            installed: true,
            ..args(&["base"])
        },
    )
    .await
    .expect("installed");
    assert_eq!(reporter.take(), ["zeta\n"]);

    for (filter_args, expected) in [
        (
            Args {
                include_optional: true,
                ..args(&["base"])
            },
            "alpha\noptional-user\nrecommended-build-user\nzeta\n",
        ),
        (
            Args {
                include_build: true,
                ..args(&["base"])
            },
            "alpha\nbuild-user\nrecommended-build-user\nzeta\n",
        ),
        (
            Args {
                include_test: true,
                ..args(&["base"])
            },
            "alpha\nrecommended-build-user\ntest-user\nzeta\n",
        ),
        (
            Args {
                include_build: true,
                skip_recommended: true,
                ..args(&["base"])
            },
            "alpha\nbuild-user\n",
        ),
    ] {
        uses::run(&ctx, filter_args).await.expect("filtered uses");
        assert_eq!(reporter.take(), [expected]);
    }
}

#[tokio::test]
async fn empty_result_emits_nothing_and_missing_target_is_typed() {
    let (_temp, ctx, reporter) = context(GRAPH);
    uses::run(&ctx, args(&["top"])).await.expect("empty result");
    assert!(reporter.take().is_empty());

    let error = uses::run(&ctx, args(&["missing"]))
        .await
        .expect_err("missing target");
    match error {
        OpError::MissingFormula { name } => assert_eq!(name, "missing"),
        other => panic!("expected missing formula, got {other:?}"),
    }
}
