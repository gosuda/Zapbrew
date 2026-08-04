use std::collections::HashMap;
use std::io;
use std::os::unix::fs::symlink;
use std::str::FromStr;
use std::sync::{Arc, Mutex};

use camino::{Utf8Path, Utf8PathBuf};
use serde_json::{Value, json};
use tempfile::TempDir;
use zapbrew_api::{CaskCatalog, Catalog};
use zapbrew_ops::info::{self, Args};
use zapbrew_ops::{Ctx, Reporter};
use zapbrew_prefix::{CommandOutput, CommandRunner, CommandSpec, Env, EnvDetectInput, Keg, Tab};
use zapbrew_types::{FormulaName, PkgVersion};

struct PanicRunner;
impl CommandRunner for PanicRunner {
    fn run(&self, spec: &CommandSpec) -> Result<CommandOutput, io::Error> {
        panic!("host command must not run: {:?}", spec.program())
    }
}

#[derive(Default)]
struct RecordingReporter(Mutex<Vec<String>>, bool, bool);
impl RecordingReporter {
    fn with(quiet: bool, verbose: bool) -> Self {
        Self(Mutex::new(Vec::new()), quiet, verbose)
    }
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
    fn is_quiet(&self) -> bool {
        self.1
    }
    fn is_verbose(&self) -> bool {
        self.2
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

fn formulae() -> Vec<Value> {
    vec![
        json!({
            "name": "sample",
            "full_name": "homebrew/core/sample",
            "tap": "homebrew/core",
            "oldnames": ["sample-old"],
            "aliases": ["samp"],
            "desc": "Base description",
            "license": "MIT",
            "homepage": "https://example.test/sample",
            "versions": {"stable": "2.0", "bottle": true},
            "revision": 2,
            "bottle": {
                "stable": {
                    "rebuild": 0,
                    "root_url": "https://example.test/bottles",
                    "files": {
                        "x86_64_linux": {
                            "cellar": ":any_skip_relocation",
                            "url": "https://example.test/sample.tar.gz",
                            "sha256": "0000000000000000000000000000000000000000000000000000000000000000"
                        }
                    }
                }
            },
            "keg_only": true,
            "keg_only_reason": {"reason": "versioned_formula", "explanation": "versioned"},
            "dependencies": ["required-dep"],
            "build_dependencies": ["build-dep"],
            "recommended_dependencies": ["recommended-dep"],
            "optional_dependencies": ["optional-dep"],
            "test_dependencies": ["test-dep"],
            "caveats": "Use $HOMEBREW_PREFIX/bin and @@HOMEBREW_PREFIX@@/share.",
            "ruby_source_path": "Formula/s/sample.rb",
            "variations": {"x86_64_linux": {"desc": "Host description"}}
        }),
        json!({
            "name": "plain",
            "full_name": "vendor/tools/plain",
            "tap": "vendor/tools",
            "desc": "Plain formula",
            "homepage": "https://example.test/plain",
            "versions": {"stable": "1.0", "bottle": false},
            "ruby_source_path": "Formula/plain.rb"
        }),
    ]
}

fn context(environment: Env, formulae: &[Value]) -> (Ctx, Arc<RecordingReporter>) {
    context_flags(environment, formulae, RecordingReporter::default())
}

fn context_flags(
    environment: Env,
    formulae: &[Value],
    recording: RecordingReporter,
) -> (Ctx, Arc<RecordingReporter>) {
    let payload = serde_json::to_vec(formulae).expect("formula payload");
    let catalog =
        Arc::new(Catalog::from_payload(&payload, &environment.bottle_tag).expect("catalog"));
    let casks =
        Arc::new(CaskCatalog::from_payload(b"[]", &environment.bottle_tag).expect("cask catalog"));
    let recording = Arc::new(recording);
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

fn keg(env: &Env, version: &str, installed_on_request: bool, bytes: &[u8]) -> Keg {
    let keg = Keg::new(
        &env.cellar,
        FormulaName::from_str("sample").expect("formula name"),
        PkgVersion::from_str(version).expect("pkg version"),
    )
    .expect("keg");
    std::fs::create_dir_all(keg.path().join("bin")).expect("keg bin");
    std::fs::write(keg.path().join("bin/sample"), bytes).expect("keg file");
    Tab {
        installed_on_request,
        ..Tab::default()
    }
    .write(keg.receipt_path())
    .expect("tab");
    keg
}

fn link_keg(environment: &Env, keg: &Keg) {
    std::fs::create_dir_all(&environment.linked).expect("linked dir");
    symlink(keg.path(), environment.linked.join("sample")).expect("linked keg");
}

fn optlink_keg(environment: &Env, keg: &Keg) {
    std::fs::create_dir_all(environment.prefix.join("opt")).expect("opt dir");
    symlink(keg.path(), environment.prefix.join("opt").join("sample")).expect("opt-linked keg");
}

async fn render_sample(environment: &Env) -> String {
    let root = environment.prefix.clone();
    let (ctx, reporter) = context(environment.clone(), &formulae());
    let before = fingerprint(&root);
    info::run(
        &ctx,
        Args {
            names: vec!["sample".to_owned()],
            json_v2: false,
        },
    )
    .await
    .expect("text info");
    assert_eq!(
        fingerprint(&root),
        before,
        "info must not mutate installed state"
    );
    events(&reporter, &root)
}

fn events(reporter: &RecordingReporter, root: &Utf8Path) -> String {
    reporter
        .take()
        .join("\n---\n")
        .replace(root.as_str(), "<ROOT>")
}

fn fingerprint(root: &Utf8Path) -> Vec<(Utf8PathBuf, Vec<u8>)> {
    fn visit(path: &Utf8Path, root: &Utf8Path, entries: &mut Vec<(Utf8PathBuf, Vec<u8>)>) {
        let mut children = std::fs::read_dir(path)
            .expect("read tree")
            .map(|entry| entry.expect("tree entry"))
            .collect::<Vec<_>>();
        children.sort_by_key(std::fs::DirEntry::path);
        for child in children {
            let path = Utf8PathBuf::from_path_buf(child.path()).expect("utf8 tree path");
            let relative = path.strip_prefix(root).expect("relative").to_path_buf();
            let metadata = std::fs::symlink_metadata(&path).expect("tree metadata");
            if metadata.file_type().is_symlink() {
                let target = std::fs::read_link(&path).expect("read symlink");
                entries.push((relative, format!("L:{}", target.display()).into_bytes()));
            } else if metadata.is_dir() {
                entries.push((relative, b"D".to_vec()));
                visit(&path, root, entries);
            } else {
                let mut value = b"F:".to_vec();
                value.extend(std::fs::read(&path).expect("read file"));
                entries.push((relative, value));
            }
        }
    }

    let mut entries = Vec::new();
    if root.exists() {
        visit(root, root, &mut entries);
    }
    entries
}

#[tokio::test]
async fn full_not_installed_info_is_exact() {
    let temp = TempDir::new().expect("temp");
    let environment = env(&temp);
    let root = environment.prefix.clone();
    let (ctx, reporter) = context(environment, &formulae());
    let before = fingerprint(&root);

    info::run(
        &ctx,
        Args {
            names: vec!["samp".to_owned()],
            json_v2: false,
        },
    )
    .await
    .expect("text info");
    assert_eq!(
        fingerprint(&root),
        before,
        "info must not mutate the prefix"
    );
    insta::assert_snapshot!(events(&reporter, &root), @r"
    ohai:homebrew/core/sample: stable 2.0 (bottled) [keg-only]
    ---
    print:Host description
    ---
    print:https://example.test/sample
    ---
    print:Aliases: samp
    ---
    print:Old Names: sample-old
    ---
    print:Not installed
    ---
    print:From: https://github.com/Homebrew/homebrew-core/blob/HEAD/Formula/s/sample.rb
    ---
    print:License: MIT
    ---
    ohai:Dependencies
    ---
    print:Build (1): build-dep
    ---
    print:Required (1): required-dep
    ---
    print:Recommended (1): recommended-dep
    ---
    print:Optional (1): optional-dep
    ---
    ohai:Caveats
    ---
    print:Use <ROOT>/bin and <ROOT>/share.
    ");
}

#[tokio::test]
async fn installed_revision_link_selection_status_and_multiple_name_separator_are_exact() {
    let temp = TempDir::new().expect("temp");
    let environment = env(&temp);
    let linked = keg(&environment, "1.0", true, b"old");
    keg(&environment, "2.0_2", false, b"new-version");
    optlink_keg(&environment, &linked);
    link_keg(&environment, &linked);
    let root = environment.prefix.clone();
    let (ctx, reporter) = context(environment, &formulae());
    let before = fingerprint(&root);

    info::run(
        &ctx,
        Args {
            names: vec!["sample".to_owned(), "plain".to_owned()],
            json_v2: false,
        },
    )
    .await
    .expect("installed info");
    assert_eq!(
        fingerprint(&root),
        before,
        "info must not mutate installed state"
    );
    insta::assert_snapshot!(events(&reporter, &root), @r"
    ohai:homebrew/core/sample: stable 2.0 (bottled) [keg-only]
    ---
    print:Host description
    ---
    print:https://example.test/sample
    ---
    print:Aliases: samp
    ---
    print:Old Names: sample-old
    ---
    print:Installed (on request)
    ---
    print:From: https://github.com/Homebrew/homebrew-core/blob/HEAD/Formula/s/sample.rb
    ---
    print:License: MIT
    ---
    ohai:Installed Versions
    ---
    print:homebrew/core/sample 2.0_2 (2 files, 652B)
    ---
    print:homebrew/core/sample 1.0   (2 files, 643B) [Linked]
    ---
    ohai:Dependencies
    ---
    print:Build (1): build-dep
    ---
    print:Required (1): required-dep
    ---
    print:Recommended (1): recommended-dep
    ---
    print:Optional (1): optional-dep
    ---
    ohai:Caveats
    ---
    print:Use <ROOT>/bin and <ROOT>/share.
    ---
    print:
    ---
    ohai:vendor/tools/plain: stable 1.0
    ---
    print:Plain formula
    ---
    print:https://example.test/plain
    ---
    print:Not installed
    ---
    print:From: https://github.com/vendor/homebrew-tools/blob/HEAD/Formula/plain.rb
    ");
}

#[tokio::test]
async fn status_prefers_optlinked_over_linked_when_flags_disagree() {
    let temp = TempDir::new().expect("temp");
    let environment = env(&temp);
    let opt = keg(&environment, "1.0", true, b"old-opt");
    let linked = keg(&environment, "2.0_2", false, b"new-version");
    optlink_keg(&environment, &opt);
    link_keg(&environment, &linked);
    insta::assert_snapshot!(render_sample(&environment).await, @r"
    ohai:homebrew/core/sample: stable 2.0 (bottled) [keg-only]
    ---
    print:Host description
    ---
    print:https://example.test/sample
    ---
    print:Aliases: samp
    ---
    print:Old Names: sample-old
    ---
    print:Installed (on request)
    ---
    print:From: https://github.com/Homebrew/homebrew-core/blob/HEAD/Formula/s/sample.rb
    ---
    print:License: MIT
    ---
    ohai:Installed Versions
    ---
    print:homebrew/core/sample 2.0_2 (2 files, 652B) [Linked]
    ---
    ohai:Dependencies
    ---
    print:Build (1): build-dep
    ---
    print:Required (1): required-dep
    ---
    print:Recommended (1): recommended-dep
    ---
    print:Optional (1): optional-dep
    ---
    ohai:Caveats
    ---
    print:Use <ROOT>/bin and <ROOT>/share.
    ");
}

#[tokio::test]
async fn status_falls_back_to_linked_when_no_optlink() {
    let temp = TempDir::new().expect("temp");
    let environment = env(&temp);
    let linked = keg(&environment, "1.0", false, b"old");
    keg(&environment, "2.0_2", true, b"new-version");
    link_keg(&environment, &linked);
    insta::assert_snapshot!(render_sample(&environment).await, @r"
    ohai:homebrew/core/sample: stable 2.0 (bottled) [keg-only]
    ---
    print:Host description
    ---
    print:https://example.test/sample
    ---
    print:Aliases: samp
    ---
    print:Old Names: sample-old
    ---
    print:Installed (as dependency)
    ---
    print:From: https://github.com/Homebrew/homebrew-core/blob/HEAD/Formula/s/sample.rb
    ---
    print:License: MIT
    ---
    ohai:Installed Versions
    ---
    print:homebrew/core/sample 2.0_2 (2 files, 651B)
    ---
    print:homebrew/core/sample 1.0   (2 files, 644B) [Linked]
    ---
    ohai:Dependencies
    ---
    print:Build (1): build-dep
    ---
    print:Required (1): required-dep
    ---
    print:Recommended (1): recommended-dep
    ---
    print:Optional (1): optional-dep
    ---
    ohai:Caveats
    ---
    print:Use <ROOT>/bin and <ROOT>/share.
    ");
}

#[tokio::test]
async fn status_uses_sole_installed_keg() {
    let temp = TempDir::new().expect("temp");
    let environment = env(&temp);
    keg(&environment, "1.0", true, b"sole");
    insta::assert_snapshot!(render_sample(&environment).await, @r"
    ohai:homebrew/core/sample: stable 2.0 (bottled) [keg-only]
    ---
    print:Host description
    ---
    print:https://example.test/sample
    ---
    print:Aliases: samp
    ---
    print:Old Names: sample-old
    ---
    print:Installed (on request)
    ---
    print:From: https://github.com/Homebrew/homebrew-core/blob/HEAD/Formula/s/sample.rb
    ---
    print:License: MIT
    ---
    ohai:Installed Versions
    ---
    print:homebrew/core/sample 1.0 (2 files, 644B)
    ---
    ohai:Dependencies
    ---
    print:Build (1): build-dep
    ---
    print:Required (1): required-dep
    ---
    print:Recommended (1): recommended-dep
    ---
    print:Optional (1): optional-dep
    ---
    ohai:Caveats
    ---
    print:Use <ROOT>/bin and <ROOT>/share.
    ");
}

#[tokio::test]
async fn status_falls_back_to_latest_when_nothing_linked() {
    let temp = TempDir::new().expect("temp");
    let environment = env(&temp);
    keg(&environment, "1.0", false, b"old");
    keg(&environment, "2.0_2", true, b"new-version");
    insta::assert_snapshot!(render_sample(&environment).await, @r"
    ohai:homebrew/core/sample: stable 2.0 (bottled) [keg-only]
    ---
    print:Host description
    ---
    print:https://example.test/sample
    ---
    print:Aliases: samp
    ---
    print:Old Names: sample-old
    ---
    print:Installed (on request)
    ---
    print:From: https://github.com/Homebrew/homebrew-core/blob/HEAD/Formula/s/sample.rb
    ---
    print:License: MIT
    ---
    ohai:Installed Versions
    ---
    print:homebrew/core/sample 2.0_2 (2 files, 651B)
    ---
    ohai:Dependencies
    ---
    print:Build (1): build-dep
    ---
    print:Required (1): required-dep
    ---
    print:Recommended (1): recommended-dep
    ---
    print:Optional (1): optional-dep
    ---
    ohai:Caveats
    ---
    print:Use <ROOT>/bin and <ROOT>/share.
    ");
}

#[tokio::test]
async fn status_uses_optlinked_intent_for_keg_only_formula() {
    let temp = TempDir::new().expect("temp");
    let environment = env(&temp);
    let opt = keg(&environment, "1.0", true, b"old-opt");
    keg(&environment, "2.0_2", false, b"new-version");
    optlink_keg(&environment, &opt);
    insta::assert_snapshot!(render_sample(&environment).await, @r"
    ohai:homebrew/core/sample: stable 2.0 (bottled) [keg-only]
    ---
    print:Host description
    ---
    print:https://example.test/sample
    ---
    print:Aliases: samp
    ---
    print:Old Names: sample-old
    ---
    print:Installed (on request)
    ---
    print:From: https://github.com/Homebrew/homebrew-core/blob/HEAD/Formula/s/sample.rb
    ---
    print:License: MIT
    ---
    ohai:Installed Versions
    ---
    print:homebrew/core/sample 2.0_2 (2 files, 652B)
    ---
    ohai:Dependencies
    ---
    print:Build (1): build-dep
    ---
    print:Required (1): required-dep
    ---
    print:Recommended (1): recommended-dep
    ---
    print:Optional (1): optional-dep
    ---
    ohai:Caveats
    ---
    print:Use <ROOT>/bin and <ROOT>/share.
    ");
}

#[tokio::test]
async fn json_v2_preserves_merged_raw_objects_and_catalog_order() {
    let temp = TempDir::new().expect("temp");
    let fixture = formulae();
    let (ctx, reporter) = context(env(&temp), &fixture);

    info::run(
        &ctx,
        Args {
            names: vec!["samp".to_owned()],
            json_v2: true,
        },
    )
    .await
    .expect("named JSON");
    let named = reporter.take();
    assert_eq!(named.len(), 1);
    let named: Value = serde_json::from_str(named[0].strip_prefix("print:").expect("print event"))
        .expect("named JSON output");
    let mut expected = fixture[0].clone();
    expected
        .as_object_mut()
        .expect("formula object")
        .remove("variations");
    expected["desc"] = json!("Host description");
    assert_eq!(named["formulae"], json!([expected]));
    assert_eq!(named["casks"], json!([]));

    info::run(
        &ctx,
        Args {
            names: Vec::new(),
            json_v2: true,
        },
    )
    .await
    .expect("all catalog JSON");
    let all = reporter.take();
    assert_eq!(all.len(), 1);
    let all: Value = serde_json::from_str(all[0].strip_prefix("print:").expect("print event"))
        .expect("all JSON output");
    assert_eq!(all["formulae"].as_array().expect("formula array").len(), 2);
    assert_eq!(all["formulae"][0]["name"], "sample");
    assert_eq!(all["formulae"][1]["name"], "plain");
    assert_eq!(all["casks"], json!([]));
}

#[tokio::test]
async fn text_without_names_refuses_and_missing_name_is_typed() {
    let temp = TempDir::new().expect("temp");
    let (ctx, reporter) = context(env(&temp), &formulae());

    let unspecified = info::run(&ctx, Args::default())
        .await
        .expect_err("unspecified text info");
    assert!(matches!(unspecified, zapbrew_ops::OpError::Refusal { .. }));
    assert_eq!(
        unspecified.to_string(),
        "this command requires a formula or cask argument"
    );

    let missing = info::run(
        &ctx,
        Args {
            names: vec!["missing".to_owned()],
            json_v2: false,
        },
    )
    .await
    .expect_err("missing formula");
    assert!(matches!(
        missing,
        zapbrew_ops::OpError::MissingFormula { ref name } if name == "missing"
    ));
    assert!(reporter.take().is_empty());
}

#[tokio::test]
async fn quiet_info_drops_caveats() {
    let temp = TempDir::new().expect("temp");
    let (ctx, reporter) = context_flags(
        env(&temp),
        &formulae(),
        RecordingReporter::with(true, false),
    );

    info::run(
        &ctx,
        Args {
            names: vec!["sample".to_owned()],
            json_v2: false,
        },
    )
    .await
    .expect("quiet info");
    let messages = reporter.take();
    assert!(!messages.iter().any(|line| line == "ohai:Caveats"));
    assert!(!messages.iter().any(|line| line.starts_with("print:Use ")));
}
