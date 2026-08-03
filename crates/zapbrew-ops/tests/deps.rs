use std::collections::HashMap;
use std::io;
use std::sync::{Arc, Mutex};

use camino::Utf8PathBuf;
use tempfile::TempDir;
use zapbrew_api::{CaskCatalog, Catalog};
use zapbrew_ops::deps::{self, Args};
use zapbrew_ops::{Ctx, OpError, Reporter};
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
        ..Args::default()
    }
}

const GRAPH: &[u8] = br#"[
  {"name":"left-root","full_name":"left-root","versions":{"stable":"1"},"dependencies":["alpha","shared"]},
  {"name":"right-root","full_name":"right-root","versions":{"stable":"1"},"dependencies":["shared","zeta"]},
  {"name":"alpha","full_name":"alpha","versions":{"stable":"1"}},
  {"name":"shared","full_name":"shared","versions":{"stable":"1"}},
  {"name":"zeta","full_name":"zeta","versions":{"stable":"1"}}
]"#;

#[tokio::test]
async fn non_tree_defaults_to_intersection_and_union_is_sorted_unique() {
    let (_temp, ctx, reporter) = context(GRAPH);
    deps::run(&ctx, args(&["left-root", "right-root"]))
        .await
        .expect("intersection");
    assert_eq!(reporter.take(), ["shared"]);

    deps::run(
        &ctx,
        Args {
            union: true,
            ..args(&["left-root", "right-root"])
        },
    )
    .await
    .expect("union");
    assert_eq!(reporter.take(), ["alpha\nshared\nzeta"]);
}

#[tokio::test]
async fn tree_preserves_declared_order_repeats_subtrees_and_reports_cycle_after_output() {
    let payload = br#"[
      {"name":"root","full_name":"root","versions":{"stable":"1"},
       "dependencies":["left","right"],"test_dependencies":["root-test"]},
      {"name":"left","full_name":"left","versions":{"stable":"1"},
       "dependencies":["shared"],"test_dependencies":["indirect-test"]},
      {"name":"right","full_name":"right","versions":{"stable":"1"},"dependencies":["shared","root"]},
      {"name":"shared","full_name":"shared","versions":{"stable":"1"},"dependencies":["bottom"]},
      {"name":"bottom","full_name":"bottom","versions":{"stable":"1"}},
      {"name":"root-test","full_name":"root-test","versions":{"stable":"1"}},
      {"name":"indirect-test","full_name":"indirect-test","versions":{"stable":"1"}}
    ]"#;
    let (_temp, ctx, reporter) = context(payload);
    let error = deps::run(
        &ctx,
        Args {
            tree: true,
            include_test: true,
            ..args(&["root"])
        },
    )
    .await
    .expect_err("cycle");

    assert_eq!(
        reporter.take(),
        [concat!(
            "root\n",
            "├── left\n",
            "│   └── shared\n",
            "│       └── bottom\n",
            "├── right\n",
            "│   ├── shared\n",
            "│   │   └── bottom\n",
            "│   └── root (CIRCULAR DEPENDENCY)\n",
            "└── root-test"
        )]
    );
    match error {
        OpError::DependencyCycle { cycle } => assert_eq!(cycle, ["root", "right", "root"]),
        other => panic!("expected cycle error, got {other:?}"),
    }
}

#[tokio::test]
async fn empty_input_emits_nothing_and_missing_formula_is_typed() {
    let (_temp, ctx, reporter) = context(GRAPH);
    deps::run(&ctx, Args::default()).await.expect("empty");
    assert!(reporter.take().is_empty());

    let error = deps::run(&ctx, args(&["missing"]))
        .await
        .expect_err("missing formula");
    match error {
        OpError::MissingFormula { name } => assert_eq!(name, "missing"),
        other => panic!("expected missing formula, got {other:?}"),
    }
}
