use std::collections::HashMap;
use std::io;
use std::sync::{Arc, Mutex};

use camino::Utf8PathBuf;
use flate2::Compression;
use flate2::write::GzEncoder;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};
use zapbrew_api::{CaskCatalog, Catalog};
use zapbrew_ops::install;
use zapbrew_ops::reinstall;
use zapbrew_ops::{Ctx, Reporter};
use zapbrew_prefix::{CommandOutput, CommandRunner, CommandSpec, Env, EnvDetectInput, Tab};

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

fn ctx(env: Env, formulae: Vec<Value>) -> Ctx {
    ctx_with_reporter(env, formulae).0
}

fn ctx_with_reporter(env: Env, formulae: Vec<Value>) -> (Ctx, Arc<RecordingReporter>) {
    ctx_with_flags(env, formulae, RecordingReporter::default())
}

fn ctx_with_flags(
    env: Env,
    formulae: Vec<Value>,
    recording: RecordingReporter,
) -> (Ctx, Arc<RecordingReporter>) {
    let bytes = serde_json::to_vec(&formulae).expect("json");
    let recording = Arc::new(recording);
    (
        Ctx {
            catalog: Arc::new(Catalog::from_payload(&bytes, &env.bottle_tag).expect("catalog")),
            casks: Arc::new(CaskCatalog::from_payload(b"[]", &env.bottle_tag).expect("casks")),
            env,
            http: reqwest::Client::new(),
            commands: Arc::new(PanicRunner),
            reporter: recording.clone(),
        },
        recording,
    )
}

fn tarball(name: &str, entries: &[(&str, &[u8])]) -> Vec<u8> {
    let encoder = GzEncoder::new(Vec::new(), Compression::default());
    let mut archive = tar::Builder::new(encoder);
    for (relative, body) in entries {
        let mut header = tar::Header::new_gnu();
        header.set_size(body.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();
        archive
            .append_data(&mut header, format!("{name}/1.0/{relative}"), *body)
            .expect("tar entry");
    }
    archive.into_inner().expect("tar").finish().expect("gzip")
}

fn sha(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn formula(url: &str, digest: &str) -> Value {
    json!({
        "name": "root",
        "full_name": "root",
        "versions": {"stable": "1.0", "bottle": true},
        "bottle": {"stable": {"rebuild": 0, "root_url": "unused", "files": {
            "x86_64_linux": {"cellar": ":any_skip_relocation", "url": url, "sha256": digest}
        }}}
    })
}

async fn mount(server: &MockServer, route: &str, body: &[u8]) {
    Mock::given(method("GET"))
        .and(path(route))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(body.to_vec()))
        .expect(1)
        .mount(server)
        .await;
}

async fn initial_install(ctx: &Ctx) {
    install::run(
        ctx,
        install::Args {
            names: vec!["root".to_owned()],
            ..install::Args::default()
        },
    )
    .await
    .expect("initial install");
}

#[tokio::test]
async fn reinstall_preserves_installed_on_request_and_replaces_same_keg() {
    let server = MockServer::start().await;
    let bottle = tarball("root", &[("bin/root", b"old")]);
    let digest = sha(&bottle);
    mount(&server, "/root", &bottle).await;
    let temp = TempDir::new().expect("temp");
    let context = ctx(
        env(&temp),
        vec![formula(&format!("{}/root", server.uri()), &digest)],
    );
    initial_install(&context).await;
    let receipt = context.env.cellar.join("root/1.0/INSTALL_RECEIPT.json");
    let mut tab = Tab::load(&receipt).expect("tab");
    tab.installed_on_request = false;
    tab.write(&receipt).expect("write tab");

    reinstall::run(
        &context,
        reinstall::Args {
            names: vec!["root".to_owned()],
        },
    )
    .await
    .expect("reinstall");

    let reinstalled = Tab::load(&receipt).expect("new tab");
    assert!(!reinstalled.installed_on_request);
    assert_eq!(
        std::fs::read(context.env.cellar.join("root/1.0/bin/root")).expect("root"),
        b"old"
    );
    assert!(context.env.prefix.join("bin/root").is_symlink());
    let rack_entries = std::fs::read_dir(context.env.cellar.join("root"))
        .expect("rack")
        .count();
    assert_eq!(rack_entries, 1, "backup and staging must be gone");
}

#[tokio::test]
async fn link_failure_restores_exact_old_keg_receipt_and_links() {
    let server = MockServer::start().await;
    let old_bottle = tarball("root", &[("bin/root", b"old")]);
    let old_sha = sha(&old_bottle);
    mount(&server, "/old", &old_bottle).await;
    let temp = TempDir::new().expect("temp");
    let environment = env(&temp);
    let old_ctx = ctx(
        environment.clone(),
        vec![formula(&format!("{}/old", server.uri()), &old_sha)],
    );
    initial_install(&old_ctx).await;
    let keg = environment.cellar.join("root/1.0");
    let old_receipt = std::fs::read(keg.join("INSTALL_RECEIPT.json")).expect("receipt bytes");
    let old_binary = std::fs::read(keg.join("bin/root")).expect("old binary");

    std::fs::create_dir_all(environment.prefix.join("sbin")).expect("sbin");
    std::fs::write(environment.prefix.join("sbin/root"), b"conflict").expect("conflict");
    let new_bottle = tarball("root", &[("bin/root", b"new"), ("sbin/root", b"new-sbin")]);
    let new_sha = sha(&new_bottle);
    mount(&server, "/new", &new_bottle).await;
    let new_ctx = ctx(
        environment.clone(),
        vec![formula(&format!("{}/new", server.uri()), &new_sha)],
    );

    let error = reinstall::run(
        &new_ctx,
        reinstall::Args {
            names: vec!["root".to_owned()],
        },
    )
    .await
    .expect_err("link conflict");
    assert!(error.to_string().contains("Could not link root"));
    assert_eq!(
        std::fs::read(keg.join("INSTALL_RECEIPT.json")).expect("restored receipt"),
        old_receipt
    );
    assert_eq!(
        std::fs::read(keg.join("bin/root")).expect("restored binary"),
        old_binary
    );
    assert!(environment.prefix.join("bin/root").is_symlink());
    assert_eq!(
        std::fs::read(environment.prefix.join("sbin/root")).expect("conflict preserved"),
        b"conflict"
    );
    assert!(environment.prefix.join("opt/root").is_symlink());
    assert!(environment.linked.join("root").is_symlink());
}

#[tokio::test]
async fn reinstall_requires_installed_formula() {
    let temp = TempDir::new().expect("temp");
    let context = ctx(env(&temp), vec![formula("http://unused", &"0".repeat(64))]);
    let error = reinstall::run(
        &context,
        reinstall::Args {
            names: vec!["root".to_owned()],
        },
    )
    .await
    .expect_err("not installed");
    assert_eq!(error.to_string(), "root is not installed");
}

#[tokio::test]
async fn reinstall_caveats_and_summary_match_install_output() {
    let server = MockServer::start().await;
    let body = vec![b'x'; 2048];
    let bottle = tarball("root", &[("bin/root", &body)]);
    let digest = sha(&bottle);
    mount(&server, "/root", &bottle).await;
    let temp = TempDir::new().expect("temp");
    let environment = env(&temp);
    let mut root = formula(&format!("{}/root", server.uri()), &digest);
    root["caveats"] = json!("$HOMEBREW_PREFIX|#{HOMEBREW_PREFIX}|@@HOMEBREW_PREFIX@@");
    let (context, reporter) = ctx_with_reporter(environment, vec![root]);

    initial_install(&context).await;
    let install_messages = reporter.take();
    reinstall::run(
        &context,
        reinstall::Args {
            names: vec!["root".to_owned()],
        },
    )
    .await
    .expect("reinstall");
    let reinstall_messages = reporter.take();

    let install_output = &install_messages[install_messages.len() - 3..];
    let reinstall_output = &reinstall_messages[reinstall_messages.len() - 3..];
    assert_eq!(reinstall_output, install_output);
    assert_eq!(reinstall_output[0], "ohai:Caveats");
    assert_eq!(
        reinstall_output[1],
        format!("print:{0}|{0}|{0}", context.env.prefix)
    );
    assert!(reinstall_output[2].ends_with("KB"));
}

#[tokio::test]
async fn quiet_reinstall_drops_caveats() {
    let server = MockServer::start().await;
    let bottle = tarball("root", &[("bin/root", b"old")]);
    let digest = sha(&bottle);
    Mock::given(method("GET"))
        .and(path("/root"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(bottle.clone()))
        .expect(1)
        .mount(&server)
        .await;
    let temp = TempDir::new().expect("temp");
    let mut root = formula(&format!("{}/root", server.uri()), &digest);
    root["caveats"] = json!("Config lives in $HOMEBREW_PREFIX/etc.");
    let (context, reporter) =
        ctx_with_flags(env(&temp), vec![root], RecordingReporter::with(true, false));

    initial_install(&context).await;
    reporter.take();
    reinstall::run(
        &context,
        reinstall::Args {
            names: vec!["root".to_owned()],
        },
    )
    .await
    .expect("reinstall");
    let messages = reporter.take();
    assert!(!messages.iter().any(|line| line == "ohai:Caveats"));
    assert!(
        !messages
            .iter()
            .any(|line| line.starts_with("print:Config lives in"))
    );
}
