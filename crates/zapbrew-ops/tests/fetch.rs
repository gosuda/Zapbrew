use std::collections::HashMap;
use std::io;
use std::sync::{Arc, Mutex};

use camino::Utf8PathBuf;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};
use zapbrew_api::{CaskCatalog, Catalog};
use zapbrew_ops::fetch::{self, Args};
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
            ]),
            available_parallelism: 2,
        },
        &PanicRunner,
    )
    .expect("env")
}

fn ctx(env: Env, formulae: Vec<Value>) -> (Ctx, Arc<RecordingReporter>) {
    let bytes = serde_json::to_vec(&formulae).expect("json");
    let reporter = Arc::new(RecordingReporter::default());
    (
        Ctx {
            catalog: Arc::new(Catalog::from_payload(&bytes, &env.bottle_tag).expect("catalog")),
            casks: Arc::new(CaskCatalog::from_payload(b"[]", &env.bottle_tag).expect("casks")),
            env,
            http: reqwest::Client::new(),
            commands: Arc::new(PanicRunner),
            reporter: reporter.clone(),
        },
        reporter,
    )
}

fn sha(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn formula(name: &str, url: &str, digest: &str) -> Value {
    json!({
        "name": name,
        "full_name": name,
        "versions": {"stable": "1.0", "bottle": true},
        "bottle": {"stable": {"rebuild": 0, "root_url": "unused", "files": {
            "x86_64_linux": {"cellar": ":any", "url": url, "sha256": digest}
        }}}
    })
}

#[tokio::test]
async fn fresh_then_reused_print_exact_path_and_checksum() {
    let server = MockServer::start().await;
    let body = b"fetch-body";
    let digest = sha(body);
    Mock::given(method("GET"))
        .and(path("/root"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(body.as_slice()))
        .expect(1)
        .mount(&server)
        .await;
    let temp = TempDir::new().expect("temp");
    let (ctx, reporter) = ctx(
        env(&temp),
        vec![formula("root", &format!("{}/root", server.uri()), &digest)],
    );

    fetch::run(
        &ctx,
        Args {
            names: vec!["root".to_owned()],
            deps: false,
        },
    )
    .await
    .expect("fresh fetch");
    let fresh = reporter.take();
    assert_eq!(fresh[0], "ohai:Fetching root");
    assert_eq!(fresh[1], format!("oh1:Downloading {}/root", server.uri()));
    assert!(fresh[2].starts_with("print:Downloaded to: "));
    assert_eq!(fresh[3], format!("print:SHA-256: {digest}"));

    fetch::run(
        &ctx,
        Args {
            names: vec!["root".to_owned()],
            deps: false,
        },
    )
    .await
    .expect("reused fetch");
    let reused = reporter.take();
    assert!(reused[2].starts_with("print:Already downloaded: "));
    assert_eq!(reused[3], format!("print:SHA-256: {digest}"));
    assert!(!ctx.env.cellar.exists());
}

#[tokio::test]
async fn deps_fetches_postorder_with_deterministic_messages() {
    let server = MockServer::start().await;
    let dep_body = b"dep";
    let root_body = b"root";
    let dep_sha = sha(dep_body);
    let root_sha = sha(root_body);
    for (route, body) in [
        ("/dep", dep_body.as_slice()),
        ("/root", root_body.as_slice()),
    ] {
        Mock::given(method("GET"))
            .and(path(route))
            .respond_with(ResponseTemplate::new(200).set_body_bytes(body))
            .expect(1)
            .mount(&server)
            .await;
    }
    let mut root = formula("root", &format!("{}/root", server.uri()), &root_sha);
    root["dependencies"] = json!(["dep"]);
    let dep = formula("dep", &format!("{}/dep", server.uri()), &dep_sha);
    let temp = TempDir::new().expect("temp");
    let (ctx, reporter) = ctx(env(&temp), vec![root, dep]);

    fetch::run(
        &ctx,
        Args {
            names: vec!["root".to_owned()],
            deps: true,
        },
    )
    .await
    .expect("fetch deps");

    let messages = reporter.take();
    assert_eq!(
        &messages[..4],
        vec![
            "ohai:Fetching dep".to_owned(),
            format!("oh1:Downloading {}/dep", server.uri()),
            "ohai:Fetching root".to_owned(),
            format!("oh1:Downloading {}/root", server.uri()),
        ]
    );
    assert_eq!(
        messages
            .iter()
            .filter(|message| message.starts_with("print:SHA-256:"))
            .count(),
        2
    );
}

#[tokio::test]
async fn missing_bottle_errors_without_cache_or_cellar_mutation() {
    let temp = TempDir::new().expect("temp");
    let environment = env(&temp);
    let cache = environment.cache.clone();
    let cellar = environment.cellar.clone();
    let mut absent = formula("absent", "http://unused", &"0".repeat(64));
    absent
        .as_object_mut()
        .expect("formula object")
        .remove("bottle");
    let (ctx, _) = ctx(environment, vec![absent]);

    let error = fetch::run(
        &ctx,
        Args {
            names: vec!["absent".to_owned()],
            deps: false,
        },
    )
    .await
    .expect_err("no bottle");

    assert_eq!(
        error.to_string(),
        "absent: no bottle available for x86_64_linux. brew can build from source; zapbrew cannot."
    );
    assert!(!cache.exists());
    assert!(!cellar.exists());
}
