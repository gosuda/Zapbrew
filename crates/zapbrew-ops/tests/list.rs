use std::collections::HashMap;
use std::io;
use std::os::unix::fs::symlink;
use std::str::FromStr;
use std::sync::{Arc, Mutex};

use camino::{Utf8Path, Utf8PathBuf};
use serde_json::{Value, json};
use tempfile::TempDir;
use zapbrew_api::{CaskCatalog, Catalog};
use zapbrew_ops::list::{self, Args};
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
    let payload = serde_json::to_vec(&formulae).expect("formula payload");
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

fn formula(name: &str, aliases: &[&str]) -> Value {
    json!({
        "name": name,
        "full_name": name,
        "aliases": aliases,
        "versions": {"stable": "1.0", "bottle": false}
    })
}

fn keg(env: &Env, name: &str, version: &str) -> Keg {
    let keg = Keg::new(
        &env.cellar,
        FormulaName::from_str(name).expect("formula name"),
        PkgVersion::from_str(version).expect("pkg version"),
    )
    .expect("keg");
    std::fs::create_dir_all(keg.path()).expect("keg dir");
    Tab::default().write(keg.receipt_path()).expect("tab");
    keg
}

fn events(reporter: &RecordingReporter, root: &Utf8Path) -> String {
    reporter
        .take()
        .join("\n---\n")
        .replace(root.as_str(), "<ROOT>")
}

fn fingerprint(root: &Utf8Path) -> Vec<String> {
    fn visit(path: &Utf8Path, root: &Utf8Path, entries: &mut Vec<String>) {
        let mut children = std::fs::read_dir(path)
            .expect("read tree")
            .map(|entry| entry.expect("tree entry"))
            .collect::<Vec<_>>();
        children.sort_by_key(std::fs::DirEntry::path);
        for child in children {
            let path = Utf8PathBuf::from_path_buf(child.path()).expect("utf8 tree path");
            let relative = path.strip_prefix(root).expect("tree relative");
            let metadata = std::fs::symlink_metadata(&path).expect("tree metadata");
            if metadata.file_type().is_symlink() {
                entries.push(format!(
                    "L:{relative}->{:?}",
                    std::fs::read_link(&path).expect("read link")
                ));
            } else if metadata.is_dir() {
                entries.push(format!("D:{relative}"));
                visit(&path, root, entries);
            } else {
                entries.push(format!(
                    "F:{relative}:{:?}",
                    std::fs::read(&path).expect("read file")
                ));
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
async fn empty_installed_state_emits_nothing_and_creates_nothing() {
    let temp = TempDir::new().expect("temp");
    let environment = env(&temp);
    let prefix = environment.prefix.clone();
    let (ctx, reporter) = context(environment, Vec::new());

    list::run(
        &ctx,
        Args {
            width: 80,
            ..Args::default()
        },
    )
    .await
    .expect("empty list");

    assert!(reporter.take().is_empty());
    assert!(!prefix.exists(), "list must not create the prefix");
}

#[tokio::test]
async fn columns_width_zero_oneline_and_semantic_versions_are_exact() {
    let temp = TempDir::new().expect("temp");
    let environment = env(&temp);
    let names = ["gamma", "alpha", "epsilon", "delta", "beta"];
    for name in names {
        keg(&environment, name, "1.0");
    }
    keg(&environment, "alpha", "1.10");
    keg(&environment, "alpha", "1.2_1");
    keg(&environment, "alpha", "1.2");
    let formulae = names
        .iter()
        .map(|name| formula(name, &[]))
        .collect::<Vec<_>>();
    let root = environment.prefix.clone();
    let (ctx, reporter) = context(environment, formulae);

    list::run(
        &ctx,
        Args {
            width: 22,
            ..Args::default()
        },
    )
    .await
    .expect("column list");
    insta::assert_snapshot!(events(&reporter, &root), @r"
    print:alpha       epsilon
    beta        gamma
    delta
    ");

    list::run(
        &ctx,
        Args {
            width: 0,
            ..Args::default()
        },
    )
    .await
    .expect("width zero");
    insta::assert_snapshot!(events(&reporter, &root), @r"
    print:alpha
    beta
    delta
    epsilon
    gamma
    ");

    list::run(
        &ctx,
        Args {
            width: 8,
            ..Args::default()
        },
    )
    .await
    .expect("one-column fallback");
    insta::assert_snapshot!(events(&reporter, &root), @r"
    print:alpha
    beta
    delta
    epsilon
    gamma
    ");

    list::run(
        &ctx,
        Args {
            oneline: true,
            width: 80,
            ..Args::default()
        },
    )
    .await
    .expect("oneline");
    insta::assert_snapshot!(events(&reporter, &root), @r"
    print:alpha
    beta
    delta
    epsilon
    gamma
    ");

    list::run(
        &ctx,
        Args {
            versions: true,
            ..Args::default()
        },
    )
    .await
    .expect("versions");
    insta::assert_snapshot!(events(&reporter, &root), @r"
    print:alpha 1.0 1.2 1.2_1 1.10
    ---
    print:beta 1.0
    ---
    print:delta 1.0
    ---
    print:epsilon 1.0
    ---
    print:gamma 1.0
    ");
}

#[tokio::test]
async fn named_alias_uses_linked_keg_and_lists_absolute_non_directories_without_mutation() {
    let temp = TempDir::new().expect("temp");
    let environment = env(&temp);
    let linked = keg(&environment, "tool", "1.0");
    let latest = keg(&environment, "tool", "2.0");
    std::fs::create_dir_all(linked.path().join("bin")).expect("bin dir");
    std::fs::write(linked.path().join("bin/tool"), b"old").expect("old tool");
    std::fs::create_dir_all(linked.path().join("share")).expect("share dir");
    symlink("../bin/tool", linked.path().join("share/tool-link")).expect("file symlink");
    std::fs::create_dir_all(latest.path().join("bin")).expect("latest bin");
    std::fs::write(latest.path().join("bin/tool"), b"new").expect("new tool");
    std::fs::create_dir_all(&environment.linked).expect("linked dir");
    symlink(linked.path(), environment.linked.join("tool")).expect("linked keg");

    let root = environment.prefix.clone();
    let (ctx, reporter) = context(environment, vec![formula("tool", &["tool-old"])]);
    let before = fingerprint(&root);
    list::run(
        &ctx,
        Args {
            names: vec!["tool-old".to_owned()],
            ..Args::default()
        },
    )
    .await
    .expect("named list");
    assert_eq!(
        fingerprint(&root),
        before,
        "list must not mutate the prefix"
    );
    insta::assert_snapshot!(events(&reporter, &root), @r"
    print:<ROOT>/Cellar/tool/1.0/INSTALL_RECEIPT.json
    ---
    print:<ROOT>/Cellar/tool/1.0/bin/tool
    ---
    print:<ROOT>/Cellar/tool/1.0/share/tool-link
    ");

    let error = list::run(
        &ctx,
        Args {
            names: vec!["missing".to_owned()],
            ..Args::default()
        },
    )
    .await
    .expect_err("missing keg");
    assert_eq!(
        error.to_string().replace(root.as_str(), "<ROOT>"),
        "No such keg: <ROOT>/Cellar/missing"
    );
    assert!(reporter.take().is_empty());
}
