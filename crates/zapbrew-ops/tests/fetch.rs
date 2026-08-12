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
            ]),
            available_parallelism: 2,
        },
        &PanicRunner,
    )
    .expect("env")
}

fn ctx(env: Env, formulae: Vec<Value>) -> (Ctx, Arc<RecordingReporter>) {
    ctx_flags(env, formulae, RecordingReporter::default())
}

fn ctx_flags(
    env: Env,
    formulae: Vec<Value>,
    recording: RecordingReporter,
) -> (Ctx, Arc<RecordingReporter>) {
    let bytes = serde_json::to_vec(&formulae).expect("json");
    let reporter = Arc::new(recording);
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
            ..Default::default()
        },
    )
    .await
    .expect("fresh fetch");
    let fresh = reporter.take();
    assert_eq!(fresh[0], "ohai:Fetching root");
    assert!(fresh[1].starts_with("print:Downloaded to: "));
    assert_eq!(fresh[2], format!("print:SHA-256: {digest}"));

    fetch::run(
        &ctx,
        Args {
            names: vec!["root".to_owned()],
            deps: false,
            ..Default::default()
        },
    )
    .await
    .expect("reused fetch");
    let reused = reporter.take();
    assert!(reused[1].starts_with("print:Already downloaded: "));
    assert_eq!(reused[2], format!("print:SHA-256: {digest}"));
    assert!(!ctx.env.cellar.exists());
}

#[tokio::test]
async fn verbose_emits_download_urls() {
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
    let (ctx, reporter) = ctx_flags(
        env(&temp),
        vec![formula("root", &format!("{}/root", server.uri()), &digest)],
        RecordingReporter::with(false, true),
    );

    fetch::run(
        &ctx,
        Args {
            names: vec!["root".to_owned()],
            deps: false,
            ..Default::default()
        },
    )
    .await
    .expect("verbose fetch");
    let messages = reporter.take();
    assert_eq!(messages[0], "ohai:Fetching root");
    assert_eq!(
        messages[1],
        format!("oh1:Downloading {}/root", server.uri())
    );
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
            ..Default::default()
        },
    )
    .await
    .expect("fetch deps");

    let messages = reporter.take();
    assert_eq!(
        &messages[..2],
        vec![
            "ohai:Fetching dep".to_owned(),
            "ohai:Fetching root".to_owned(),
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
            ..Default::default()
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

fn cask(token: &str, version: &str, sha: &str, url: &str) -> Value {
    json!({
        "token": token,
        "version": version,
        "sha256": sha,
        "url": url,
    })
}

fn cask_with_old(token: &str, old: &str, version: &str, sha: &str, url: &str) -> Value {
    json!({
        "token": token,
        "old_tokens": [old],
        "version": version,
        "sha256": sha,
        "url": url,
    })
}

fn ctx_casks(
    env: Env,
    formulae: Vec<Value>,
    casks: Vec<Value>,
    recording: RecordingReporter,
) -> (Ctx, Arc<RecordingReporter>) {
    let formula_bytes = serde_json::to_vec(&formulae).expect("formula json");
    let cask_bytes = serde_json::to_vec(&casks).expect("cask json");
    let reporter = Arc::new(recording);
    (
        Ctx {
            catalog: Arc::new(
                Catalog::from_payload(&formula_bytes, &env.bottle_tag).expect("catalog"),
            ),
            casks: Arc::new(
                CaskCatalog::from_payload(&cask_bytes, &env.bottle_tag).expect("casks"),
            ),
            env,
            http: reqwest::Client::new(),
            commands: Arc::new(PanicRunner),
            reporter: reporter.clone(),
        },
        reporter,
    )
}

#[tokio::test]
async fn auto_prefers_formula_over_cask() {
    let server = MockServer::start().await;
    let body = b"formula-body";
    let digest = sha(body);
    Mock::given(method("GET"))
        .and(path("/formula"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(body.as_slice()))
        .expect(1)
        .mount(&server)
        .await;

    let temp = TempDir::new().expect("temp");
    let formulae = vec![formula(
        "firefox",
        &format!("{}/formula", server.uri()),
        &digest,
    )];
    let casks = vec![cask(
        "firefox",
        "1.0",
        &digest,
        &format!("{}/cask", server.uri()),
    )];
    let (ctx, reporter) = ctx_casks(env(&temp), formulae, casks, RecordingReporter::default());

    fetch::run(
        &ctx,
        fetch::Args {
            names: vec!["firefox".to_owned()],
            deps: false,
            mode: fetch::Mode::Auto,
        },
    )
    .await
    .expect("fetch");

    let messages = reporter.take();
    assert!(messages.iter().any(|m| m == "ohai:Fetching firefox"));
    assert!(
        messages
            .iter()
            .any(|m| m.starts_with("print:Downloaded to: "))
    );
    assert!(
        messages
            .iter()
            .any(|m| m == &format!("print:SHA-256: {digest}"))
    );
}

#[tokio::test]
async fn cask_only_ignores_formula_and_resolves_by_token() {
    let server = MockServer::start().await;
    let body = b"cask-body";
    let digest = sha(body);
    Mock::given(method("GET"))
        .and(path("/cask"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(body.as_slice()))
        .expect(1)
        .mount(&server)
        .await;

    let temp = TempDir::new().expect("temp");
    let casks = vec![cask(
        "firefox",
        "1.0",
        &digest,
        &format!("{}/cask", server.uri()),
    )];
    let (ctx, reporter) = ctx_casks(env(&temp), vec![], casks, RecordingReporter::default());

    fetch::run(
        &ctx,
        fetch::Args {
            names: vec!["firefox".to_owned()],
            deps: false,
            mode: fetch::Mode::CaskOnly,
        },
    )
    .await
    .expect("cask fetch");

    let messages = reporter.take();
    assert!(messages.iter().any(|m| m == "ohai:Fetching firefox"));
    assert!(
        messages
            .iter()
            .any(|m| m.starts_with("print:Downloaded to: "))
    );
    assert!(
        messages
            .iter()
            .any(|m| m == &format!("print:SHA-256: {digest}"))
    );
    assert!(!ctx.env.cellar.exists());
    assert!(!ctx.env.caskroom.exists());
}

#[tokio::test]
async fn formula_only_never_falls_back_to_cask() {
    let temp = TempDir::new().expect("temp");
    let mut no_bottle = formula("missing", "http://unused", &"0".repeat(64));
    no_bottle.as_object_mut().expect("object").remove("bottle");
    let casks = vec![cask("missing", "1.0", &"0".repeat(64), "http://cask")];
    let (ctx, _) = ctx_casks(
        env(&temp),
        vec![no_bottle],
        casks,
        RecordingReporter::default(),
    );

    let error = fetch::run(
        &ctx,
        fetch::Args {
            names: vec!["missing".to_owned()],
            deps: false,
            mode: fetch::Mode::FormulaOnly,
        },
    )
    .await
    .expect_err("formula-only miss");

    assert!(error.to_string().contains("no bottle available"));
    assert!(!ctx.env.cellar.exists());
    assert!(!ctx.env.caskroom.exists());
}

#[tokio::test]
async fn auto_resolves_cask_old_token_and_avoids_migration_io() {
    let server = MockServer::start().await;
    let body = b"renamed";
    let digest = sha(body);
    Mock::given(method("GET"))
        .and(path("/app.dmg"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(body.as_slice()))
        .expect(1)
        .mount(&server)
        .await;

    let temp = TempDir::new().expect("temp");
    let casks = vec![cask_with_old(
        "firefox",
        "fire-fox",
        "1.0",
        &digest,
        &format!("{}/app.dmg", server.uri()),
    )];
    let (ctx, reporter) = ctx_casks(env(&temp), vec![], casks, RecordingReporter::default());

    fetch::run(
        &ctx,
        fetch::Args {
            names: vec!["fire-fox".to_owned()],
            deps: false,
            mode: fetch::Mode::Auto,
        },
    )
    .await
    .expect("old token");

    let messages = reporter.take();
    assert!(messages.iter().any(|m| m == "ohai:Fetching firefox"));
    assert!(
        messages
            .iter()
            .any(|m| m == &format!("print:SHA-256: {digest}"))
    );
    assert!(!ctx.env.cellar.exists());
    assert!(!ctx.env.caskroom.exists());
}

#[tokio::test]
async fn auto_with_deps_accepts_cask_without_expanding_dependencies() {
    let server = MockServer::start().await;
    let body = b"cask-body";
    let digest = sha(body);
    Mock::given(method("GET"))
        .and(path("/app.dmg"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(body.as_slice()))
        .expect(1)
        .mount(&server)
        .await;

    let temp = TempDir::new().expect("temp");
    let casks = vec![cask(
        "with-deps",
        "1.0",
        &digest,
        &format!("{}/app.dmg", server.uri()),
    )];
    let (ctx, reporter) = ctx_casks(env(&temp), vec![], casks, RecordingReporter::default());

    fetch::run(
        &ctx,
        fetch::Args {
            names: vec!["with-deps".to_owned()],
            deps: true,
            mode: fetch::Mode::Auto,
        },
    )
    .await
    .expect("cask deps");

    let messages = reporter.take();
    assert!(messages.iter().any(|m| m == "ohai:Fetching with-deps"));
    assert_eq!(
        messages
            .iter()
            .filter(|m| m.starts_with("ohai:Fetching"))
            .count(),
        1
    );
}

#[tokio::test]
async fn missing_cask_url_warns_and_skips() {
    let temp = TempDir::new().expect("temp");
    let casks = vec![json!({
        "token": "no-url",
        "version": "1.0",
        "sha256": "no_check",
    })];
    let (ctx, reporter) = ctx_casks(env(&temp), vec![], casks, RecordingReporter::default());

    fetch::run(
        &ctx,
        fetch::Args {
            names: vec!["no-url".to_owned()],
            deps: false,
            mode: fetch::Mode::CaskOnly,
        },
    )
    .await
    .expect("skip");

    let messages = reporter.take();
    assert!(
        messages
            .iter()
            .any(|m| m.starts_with("opoo:Cask 'no-url' has no URL"))
    );
    assert!(!ctx.env.cellar.exists());
    assert!(!ctx.env.caskroom.exists());
}

#[tokio::test]
async fn missing_cask_checksum_warns_and_skips() {
    let temp = TempDir::new().expect("temp");
    let casks = vec![json!({
        "token": "no-sha",
        "version": "1.0",
        "url": "https://example.com/app.dmg",
    })];
    let (ctx, reporter) = ctx_casks(env(&temp), vec![], casks, RecordingReporter::default());

    fetch::run(
        &ctx,
        fetch::Args {
            names: vec!["no-sha".to_owned()],
            deps: false,
            mode: fetch::Mode::CaskOnly,
        },
    )
    .await
    .expect("skip");

    let messages = reporter.take();
    assert!(
        messages
            .iter()
            .any(|m| m.starts_with("opoo:Cask 'no-sha' has no checksum"))
    );
    assert!(!ctx.env.cellar.exists());
    assert!(!ctx.env.caskroom.exists());
}

#[tokio::test]
async fn unsafe_cask_version_errors() {
    let temp = TempDir::new().expect("temp");
    let casks = vec![cask(
        "bad",
        "../1.0",
        &"0".repeat(64),
        "https://example.com/app.dmg",
    )];
    let (ctx, _) = ctx_casks(env(&temp), vec![], casks, RecordingReporter::default());

    let error = fetch::run(
        &ctx,
        fetch::Args {
            names: vec!["bad".to_owned()],
            deps: false,
            mode: fetch::Mode::CaskOnly,
        },
    )
    .await
    .expect_err("unsafe");

    assert!(error.to_string().contains("version"));
    assert!(!ctx.env.cellar.exists());
    assert!(!ctx.env.caskroom.exists());
}

#[tokio::test]
async fn malformed_cask_checksum_errors() {
    let temp = TempDir::new().expect("temp");
    let casks = vec![cask("bad", "1.0", "not-hex", "https://example.com/app.dmg")];
    let (ctx, _) = ctx_casks(env(&temp), vec![], casks, RecordingReporter::default());

    let error = fetch::run(
        &ctx,
        fetch::Args {
            names: vec!["bad".to_owned()],
            deps: false,
            mode: fetch::Mode::CaskOnly,
        },
    )
    .await
    .expect_err("malformed");

    assert!(error.to_string().contains("invalid checksum"));
    assert!(!ctx.env.cellar.exists());
    assert!(!ctx.env.caskroom.exists());
}

#[tokio::test]
async fn cask_no_check_reports_actual_digest() {
    let server = MockServer::start().await;
    let body = b"unchecked-cask";
    let digest = sha(body);
    Mock::given(method("GET"))
        .and(path("/app.dmg"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(body.as_slice()))
        .expect(1)
        .mount(&server)
        .await;

    let temp = TempDir::new().expect("temp");
    let casks = vec![cask(
        "nocheck",
        "1.0",
        "no_check",
        &format!("{}/app.dmg", server.uri()),
    )];
    let (ctx, reporter) = ctx_casks(env(&temp), vec![], casks, RecordingReporter::default());

    fetch::run(
        &ctx,
        fetch::Args {
            names: vec!["nocheck".to_owned()],
            deps: false,
            mode: fetch::Mode::CaskOnly,
        },
    )
    .await
    .expect("no_check");

    let messages = reporter.take();
    assert!(
        messages
            .iter()
            .any(|m| m == &format!("print:SHA-256: {digest}"))
    );
}

#[tokio::test]
async fn cask_checked_verifies_and_reports_actual_digest() {
    let server = MockServer::start().await;
    let body = b"checked-cask";
    let digest = sha(body);
    Mock::given(method("GET"))
        .and(path("/app.dmg"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(body.as_slice()))
        .expect(1)
        .mount(&server)
        .await;

    let temp = TempDir::new().expect("temp");
    let casks = vec![cask(
        "checked",
        "1.0",
        &digest,
        &format!("{}/app.dmg", server.uri()),
    )];
    let (ctx, reporter) = ctx_casks(env(&temp), vec![], casks, RecordingReporter::default());

    fetch::run(
        &ctx,
        fetch::Args {
            names: vec!["checked".to_owned()],
            deps: false,
            mode: fetch::Mode::CaskOnly,
        },
    )
    .await
    .expect("checked");

    let messages = reporter.take();
    assert!(
        messages
            .iter()
            .any(|m| m == &format!("print:SHA-256: {digest}"))
    );
}

#[tokio::test]
async fn mixed_fetch_preflights_casks_before_formula_and_rejects_conflicting_checksums() {
    let server = MockServer::start().await;

    let formula_body = b"formula-body";
    let formula_digest = sha(formula_body);
    Mock::given(method("GET"))
        .and(path("/bottle"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(formula_body.as_slice()))
        .expect(0)
        .mount(&server)
        .await;

    let cask_body = b"cask-body";
    let cask_digest_a = sha(cask_body);
    let cask_digest_b = sha(b"other-body");
    Mock::given(method("GET"))
        .and(path("/app.dmg"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(cask_body.as_slice()))
        .expect(0)
        .mount(&server)
        .await;

    let temp = TempDir::new().expect("temp");
    let cask_url = format!("{}/app.dmg", server.uri());
    let casks = vec![
        cask("no-check", "1.0", "no_check", &cask_url),
        cask("checked-a", "1.0", &cask_digest_a, &cask_url),
        cask("checked-b", "1.0", &cask_digest_b, &cask_url),
    ];
    let formula_url = format!("{}/bottle", server.uri());
    let formulae = vec![formula("wget", &formula_url, &formula_digest)];
    let (ctx, _reporter) = ctx_casks(env(&temp), formulae, casks, RecordingReporter::default());

    let error = fetch::run(
        &ctx,
        fetch::Args {
            names: vec![
                "wget".to_owned(),
                "no-check".to_owned(),
                "checked-a".to_owned(),
                "checked-b".to_owned(),
            ],
            deps: false,
            mode: fetch::Mode::Auto,
        },
    )
    .await
    .expect_err("conflicting cask checksums must fail preflight");
    assert!(
        error.to_string().contains("conflicting"),
        "unexpected {error}"
    );
    assert!(!ctx.env.cellar.exists());
    assert!(!ctx.env.caskroom.exists());
}

#[tokio::test]
async fn auto_fetches_formula_and_cask_old_token_with_same_canonical() {
    let server = MockServer::start().await;

    let formula_body = b"formula-body";
    let formula_digest = sha(formula_body);
    Mock::given(method("GET"))
        .and(path("/bottle"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(formula_body.as_slice()))
        .expect(1)
        .mount(&server)
        .await;

    let cask_body = b"cask-body";
    let cask_digest = sha(cask_body);
    Mock::given(method("GET"))
        .and(path("/app.dmg"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(cask_body.as_slice()))
        .expect(1)
        .mount(&server)
        .await;

    let temp = TempDir::new().expect("temp");
    let formula_url = format!("{}/bottle", server.uri());
    let cask_url = format!("{}/app.dmg", server.uri());
    let formulae = vec![formula("shared", &formula_url, &formula_digest)];
    let casks = vec![cask_with_old(
        "shared",
        "old-shared",
        "1.0",
        &cask_digest,
        &cask_url,
    )];
    let (ctx, reporter) = ctx_casks(env(&temp), formulae, casks, RecordingReporter::default());

    fetch::run(
        &ctx,
        fetch::Args {
            names: vec!["shared".to_owned(), "old-shared".to_owned()],
            deps: false,
            mode: fetch::Mode::Auto,
        },
    )
    .await
    .expect("fetch both");

    let messages = reporter.take();
    assert_eq!(
        messages
            .iter()
            .filter(|m| m.as_str() == "ohai:Fetching shared")
            .count(),
        2,
        "both formula and cask should be fetched: {messages:?}"
    );
    assert_eq!(
        messages
            .iter()
            .filter(|m| m.starts_with("print:Downloaded to:"))
            .count(),
        2
    );
    assert_eq!(
        messages
            .iter()
            .filter(|m| m.starts_with("print:SHA-256:"))
            .count(),
        2
    );
    assert!(
        messages
            .iter()
            .any(|m| m == &format!("print:SHA-256: {formula_digest}")),
        "formula digest"
    );
    assert!(
        messages
            .iter()
            .any(|m| m == &format!("print:SHA-256: {cask_digest}")),
        "cask digest"
    );
    assert!(!ctx.env.cellar.exists());
    assert!(!ctx.env.caskroom.exists());
}

#[tokio::test]
async fn cask_only_with_deps_refuses_before_effects() {
    let temp = TempDir::new().expect("temp");
    let (ctx, _) = ctx_casks(env(&temp), vec![], vec![], RecordingReporter::default());

    let error = fetch::run(
        &ctx,
        fetch::Args {
            names: vec!["anything".to_owned()],
            deps: true,
            mode: fetch::Mode::CaskOnly,
        },
    )
    .await
    .expect_err("cask deps");

    assert_eq!(
        error.to_string(),
        "Fetching cask dependencies is not supported."
    );
    assert!(!ctx.env.cellar.exists());
    assert!(!ctx.env.caskroom.exists());
}
