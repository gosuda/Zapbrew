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

fn context_with_casks(formulae: &[u8], casks: &[u8]) -> (TempDir, Ctx, Arc<RecordingReporter>) {
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
    let catalog = Arc::new(Catalog::from_payload(formulae, &env.bottle_tag).expect("catalog"));
    let casks = Arc::new(CaskCatalog::from_payload(casks, &env.bottle_tag).expect("casks"));
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

fn install_cask(env: &Env, token: &str, version: &str) {
    std::fs::create_dir_all(env.caskroom.join(token).join(version)).expect("cask dir");
}

const CASK_FORMULAE: &[u8] = br#"[
  {"name":"dep","full_name":"dep","versions":{"stable":"1"}},
  {"name":"consumer","full_name":"consumer","versions":{"stable":"1"},"dependencies":["dep"]},
  {"name":"base","full_name":"base","versions":{"stable":"1"}}
]"#;

const CASKS: &[u8] = br#"[
  {"token":"a-cask","version":"1.0","depends_on":{"formula":["dep"]}},
  {"token":"b-cask","version":"1.0","depends_on":{"cask":["legacy-cask"]}},
  {"token":"old-cask","old_tokens":["legacy-cask"],"version":"1.0","depends_on":{"cask":["a-cask"]}},
  {"token":"intersect","version":"1.0","depends_on":{"formula":["dep"],"cask":["a-cask"]}},
  {"token":"base-consumer","version":"1.0","depends_on":{"formula":["base"]}}
]"#;
const BROKEN_FORMULAE: &[u8] = br#"[
  {"name":"dep","full_name":"dep","versions":{"stable":"1"}}
]"#;

const BROKEN_CASKS: &[u8] = br#"[
  {"token":"a-cask","version":"1.0","depends_on":{"formula":["dep"]}},
  {"token":"b-cask","version":"1.0","depends_on":{"formula":["missing-formula"],"cask":["missing-cask"]}},
  {"token":"c-cask","version":"1.0","depends_on":{"formula":["dep"],"cask":["a-cask"]}}
]"#;

#[tokio::test]
async fn cask_depending_on_formula_appears_in_uses() {
    let (_temp, ctx, reporter) = context_with_casks(CASK_FORMULAE, CASKS);
    uses::run(&ctx, args(&["dep"])).await.expect("uses dep");
    assert_eq!(reporter.take(), ["a-cask\nconsumer\nintersect\n"]);
}

#[tokio::test]
async fn cask_depending_on_cask_appears_recursively() {
    let (_temp, ctx, reporter) = context_with_casks(CASK_FORMULAE, CASKS);
    uses::run(
        &ctx,
        Args {
            recursive: true,
            ..args(&["dep"])
        },
    )
    .await
    .expect("uses dep recursive");
    assert_eq!(
        reporter.take(),
        ["a-cask\nb-cask\nconsumer\nintersect\nold-cask\n"]
    );
}

#[tokio::test]
async fn cask_target_only_reaches_cask_dependents() {
    let (_temp, ctx, reporter) = context_with_casks(CASK_FORMULAE, CASKS);
    uses::run(
        &ctx,
        Args {
            recursive: true,
            ..args(&["a-cask"])
        },
    )
    .await
    .expect("uses a-cask");
    assert_eq!(reporter.take(), ["b-cask\nintersect\nold-cask\n"]);
}

#[tokio::test]
async fn uses_resolves_cask_aliases_as_target_and_dependency() {
    let (_temp, ctx, reporter) = context_with_casks(CASK_FORMULAE, CASKS);
    // Target "legacy-cask" resolves to "old-cask"; "b-cask" depends on the old token
    // "legacy-cask", so the cask catalog resolves the edge and reports "b-cask".
    uses::run(
        &ctx,
        Args {
            recursive: true,
            ..args(&["legacy-cask"])
        },
    )
    .await
    .expect("uses legacy-cask");
    assert_eq!(reporter.take(), ["b-cask\n"]);
}

#[tokio::test]
async fn installed_filter_keeps_cask_and_formula() {
    let (_temp, ctx, reporter) = context_with_casks(CASK_FORMULAE, CASKS);
    install(&ctx.env, "consumer");
    install_cask(&ctx.env, "a-cask", "1.0");

    uses::run(
        &ctx,
        Args {
            installed: true,
            ..args(&["dep"])
        },
    )
    .await
    .expect("uses dep installed");
    assert_eq!(reporter.take(), ["a-cask\nconsumer\n"]);
}

#[tokio::test]
async fn mixed_target_intersection_requires_both_formula_and_cask() {
    let (_temp, ctx, reporter) = context_with_casks(CASK_FORMULAE, CASKS);
    // "intersect" directly depends on both the formula "dep" and the cask "a-cask".
    uses::run(&ctx, args(&["dep", "a-cask"]))
        .await
        .expect("intersection");
    assert_eq!(reporter.take(), ["intersect\n"]);
}

#[tokio::test]
async fn formula_only_uses_regression_with_casks_present() {
    let (_temp, ctx, reporter) = context_with_casks(CASK_FORMULAE, CASKS);
    // base has no cask dependents in the fixture.
    uses::run(&ctx, args(&["base"])).await.expect("uses base");
    assert_eq!(reporter.take(), ["base-consumer\n"]);
}

#[tokio::test]
async fn missing_cask_target_is_typed() {
    let (_temp, ctx, _reporter) = context_with_casks(CASK_FORMULAE, CASKS);
    let error = uses::run(&ctx, args(&["no-such-cask"]))
        .await
        .expect_err("missing cask");
    match error {
        OpError::MissingFormula { name } => assert_eq!(name, "no-such-cask"),
        other => panic!("expected missing formula error, got {other:?}"),
    }
}

#[tokio::test]
async fn missing_dependency_edges_are_ignored_in_recursive_cask_uses() {
    let (_temp, ctx, reporter) = context_with_casks(BROKEN_FORMULAE, BROKEN_CASKS);
    // Unrelated missing formula and cask edges must not abort the query.
    uses::run(
        &ctx,
        Args {
            recursive: true,
            ..args(&["a-cask"])
        },
    )
    .await
    .expect("uses a-cask despite missing unrelated edge");
    assert_eq!(reporter.take(), ["c-cask\n"]);

    // Genuine target resolution errors remain unchanged.
    let error = uses::run(&ctx, args(&["no-such-cask"]))
        .await
        .expect_err("missing cask still errors");
    match error {
        OpError::MissingFormula { name } => assert_eq!(name, "no-such-cask"),
        other => panic!("expected missing formula error, got {other:?}"),
    }
}
