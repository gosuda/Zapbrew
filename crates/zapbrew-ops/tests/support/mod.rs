#![allow(dead_code)]

use std::collections::HashMap;
use std::fs;
use std::io;
use std::str::FromStr;
use std::sync::{Arc, Mutex};

use camino::{Utf8Path, Utf8PathBuf};
use serde_json::{Value, json};
use tempfile::TempDir;
use zapbrew_api::{CaskCatalog, Catalog};
use zapbrew_ops::{Ctx, Reporter};
use zapbrew_prefix::{
    CommandOutput, CommandRunner, CommandSpec, Env, EnvDetectInput, Keg, Source, SourceVersions,
    Tab,
};
use zapbrew_types::{FormulaName, PkgVersion};

pub struct PanicRunner;

impl CommandRunner for PanicRunner {
    fn run(&self, spec: &CommandSpec) -> Result<CommandOutput, io::Error> {
        panic!("host command must not run: {:?}", spec.program())
    }
}

pub struct RecordingReporter {
    log: Mutex<Vec<String>>,
    hint: String,
}

impl Default for RecordingReporter {
    fn default() -> Self {
        Self {
            log: Mutex::new(Vec::new()),
            hint: "zapbrew".to_owned(),
        }
    }
}

impl RecordingReporter {
    /// A reporter whose self-referential hints use `hint` (e.g. `brew`).
    pub fn with_hint(hint: &str) -> Self {
        Self {
            log: Mutex::new(Vec::new()),
            hint: hint.to_owned(),
        }
    }

    fn push(&self, channel: &str, message: &str) {
        self.log
            .lock()
            .expect("reporter lock")
            .push(format!("{channel}:{message}"));
    }

    pub fn take(&self) -> Vec<String> {
        std::mem::take(&mut *self.log.lock().expect("reporter lock"))
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
    fn hint_program(&self) -> &str {
        &self.hint
    }
}

pub struct Fixture {
    pub _temp: TempDir,
    pub env: Env,
}

impl Fixture {
    pub fn new() -> Self {
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
        fs::create_dir_all(&env.cellar).expect("cellar");
        Self { _temp: temp, env }
    }

    pub fn macos(mut self) -> Self {
        self.env.bottle_tag = "sequoia".parse().expect("macOS bottle tag");
        self
    }

    pub fn context_casks(
        &self,
        casks: Vec<Value>,
        commands: Arc<dyn CommandRunner>,
        http: reqwest::Client,
    ) -> (Ctx, Arc<RecordingReporter>) {
        let payload = serde_json::to_vec(&casks).expect("cask payload");
        let catalog =
            Arc::new(Catalog::from_payload(b"[]", &self.env.bottle_tag).expect("catalog"));
        let casks = Arc::new(
            CaskCatalog::from_payload(&payload, &self.env.bottle_tag).expect("cask catalog"),
        );
        let recording = Arc::new(RecordingReporter::default());
        let reporter: Arc<dyn Reporter> = recording.clone();
        (
            Ctx {
                env: self.env.clone(),
                http,
                catalog,
                casks,
                commands,
                reporter,
            },
            recording,
        )
    }

    pub fn context(&self, formulae: Vec<Value>) -> (Ctx, Arc<RecordingReporter>) {
        let payload = serde_json::to_vec(&formulae).expect("catalog payload");
        let catalog =
            Arc::new(Catalog::from_payload(&payload, &self.env.bottle_tag).expect("catalog"));
        let casks =
            Arc::new(CaskCatalog::from_payload(b"[]", &self.env.bottle_tag).expect("casks"));
        let recording = Arc::new(RecordingReporter::default());
        let reporter: Arc<dyn Reporter> = recording.clone();
        (
            Ctx {
                env: self.env.clone(),
                http: reqwest::Client::new(),
                catalog,
                casks,
                commands: Arc::new(PanicRunner),
                reporter,
            },
            recording,
        )
    }

    /// Build a context around a caller-owned reporter, so a `brew`-hint reporter
    /// can be threaded through operations that emit self-referential hints.
    pub fn context_with_reporter(
        &self,
        formulae: Vec<Value>,
        recording: Arc<RecordingReporter>,
    ) -> Ctx {
        let payload = serde_json::to_vec(&formulae).expect("catalog payload");
        let catalog =
            Arc::new(Catalog::from_payload(&payload, &self.env.bottle_tag).expect("catalog"));
        let casks =
            Arc::new(CaskCatalog::from_payload(b"[]", &self.env.bottle_tag).expect("casks"));
        let reporter: Arc<dyn Reporter> = recording;
        Ctx {
            env: self.env.clone(),
            http: reqwest::Client::new(),
            catalog,
            casks,
            commands: Arc::new(PanicRunner),
            reporter,
        }
    }

    pub fn keg(&self, name: &str, version: &str, scheme: u32) -> Keg {
        let keg = Keg::new(
            &self.env.cellar,
            FormulaName::from_str(name).expect("formula name"),
            PkgVersion::from_str(version).expect("pkg version"),
        )
        .expect("keg");
        fs::create_dir_all(keg.path()).expect("keg dir");
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

    pub fn keg_file(&self, keg: &Keg, rel: &str, contents: &str) {
        write(&keg.path().join(rel), contents);
    }
}

pub fn formula(name: &str, version: &str, scheme: u32) -> Value {
    json!({
        "name": name,
        "full_name": name,
        "versions": {"stable": version, "bottle": true},
        "revision": 0,
        "version_scheme": scheme
    })
}

pub fn keg_only_formula(
    name: &str,
    version: &str,
    scheme: u32,
    reason: &str,
    explanation: &str,
) -> Value {
    let mut value = formula(name, version, scheme);
    value["keg_only"] = Value::Bool(true);
    value["keg_only_reason"] = json!({"reason": reason, "explanation": explanation});
    value
}

pub fn write(path: &Utf8Path, contents: &str) {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("parent");
    }
    fs::write(path, contents).expect("write");
}

pub fn is_symlink(path: &Utf8Path) -> bool {
    fs::symlink_metadata(path)
        .map(|metadata| metadata.file_type().is_symlink())
        .unwrap_or(false)
}

pub fn fingerprint(root: &Utf8Path) -> Vec<String> {
    fn visit(root: &Utf8Path, dir: &Utf8Path, out: &mut Vec<String>) {
        let entries = fs::read_dir(dir).expect("read fingerprint directory");
        let mut entries = entries
            .map(|entry| entry.expect("entry").path())
            .collect::<Vec<_>>();
        entries.sort();
        for raw in entries {
            let path = Utf8PathBuf::from_path_buf(raw).expect("utf8 path");
            let rel = path.strip_prefix(root).unwrap_or(&path);
            let metadata = fs::symlink_metadata(&path).expect("metadata");
            if metadata.file_type().is_symlink() {
                out.push(format!(
                    "L {rel} -> {}",
                    fs::read_link(&path).expect("readlink").to_string_lossy()
                ));
            } else if metadata.is_dir() {
                out.push(format!("D {rel}"));
                visit(root, &path, out);
            } else {
                out.push(format!("F {rel} {:?}", fs::read(&path).expect("read")));
            }
        }
    }

    let mut output = Vec::new();
    visit(root, root, &mut output);
    output
}
