use std::collections::HashMap;
use std::io;
use std::sync::Arc;

use camino::Utf8PathBuf;
use flate2::Compression;
use flate2::write::GzEncoder;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};
use zapbrew_api::{CaskCatalog, Catalog};
use zapbrew_ops::install::{self, Args};
use zapbrew_ops::{Ctx, Reporter};
use zapbrew_prefix::{CommandOutput, CommandRunner, CommandSpec, Env, EnvDetectInput, LockGuard};

struct PanicRunner;
impl CommandRunner for PanicRunner {
    fn run(&self, spec: &CommandSpec) -> Result<CommandOutput, io::Error> {
        panic!("host command must not run: {:?}", spec.program())
    }
}

struct NullReporter;
impl Reporter for NullReporter {
    fn ohai(&self, _: &str) {}
    fn oh1(&self, _: &str) {}
    fn opoo(&self, _: &str) {}
    fn onoe(&self, _: &str) {}
    fn print(&self, _: &str) {}
    fn eprint(&self, _: &str) {}
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
                    root.join("prefix-with-a-name-longer-than-placeholder")
                        .to_string(),
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
    let payload = serde_json::to_vec(&formulae).expect("json");
    Ctx {
        catalog: Arc::new(Catalog::from_payload(&payload, &env.bottle_tag).expect("catalog")),
        casks: Arc::new(CaskCatalog::from_payload(b"[]", &env.bottle_tag).expect("casks")),
        env,
        http: reqwest::Client::new(),
        commands: Arc::new(PanicRunner),
        reporter: Arc::new(NullReporter),
    }
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

fn formula(name: &str, url: &str, digest: &str, cellar: &str) -> Value {
    json!({
        "name": name,
        "full_name": name,
        "versions": {"stable": "1.0", "bottle": true},
        "bottle": {"stable": {"rebuild": 0, "root_url": "unused", "files": {
            "x86_64_linux": {"cellar": cellar, "url": url, "sha256": digest}
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

fn args(names: &[&str]) -> Args {
    Args {
        names: names.iter().map(|name| (*name).to_owned()).collect(),
        ..Args::default()
    }
}

#[tokio::test]
async fn relocation_failure_removes_staging_and_formula_state() {
    let server = MockServer::start().await;
    let bytes = tarball("root", &[("bin/root", b"\x7fX@@HOMEBREW_PREFIX@@\0")]);
    let digest = sha(&bytes);
    mount(&server, "/root", &bytes).await;
    let temp = TempDir::new().expect("temp");
    let environment = env(&temp);
    let context = ctx(
        environment.clone(),
        vec![formula(
            "root",
            &format!("{}/root", server.uri()),
            &digest,
            "/home/linuxbrew/.linuxbrew/Cellar",
        )],
    );

    let error = install::run(&context, args(&["root"]))
        .await
        .expect_err("relocation fails");
    assert!(error.to_string().contains("replacement") || error.to_string().contains("relocat"));
    assert!(!environment.cellar.join("root").exists());
    assert!(!environment.prefix.join("bin/root").exists());
}

#[tokio::test]
async fn skeleton_copy_failure_removes_prior_copies_links_and_promoted_keg() {
    let server = MockServer::start().await;
    let bytes = tarball(
        "root",
        &[
            ("bin/root", b"root"),
            (".bottle/etc/a.conf", b"copied-first"),
            (".bottle/etc/z/child", b"must-fail"),
        ],
    );
    let digest = sha(&bytes);
    mount(&server, "/root", &bytes).await;
    let temp = TempDir::new().expect("temp");
    let environment = env(&temp);
    std::fs::create_dir_all(environment.prefix.join("etc")).expect("etc");
    std::fs::write(environment.prefix.join("etc/z"), b"preexisting").expect("blocker");
    let context = ctx(
        environment.clone(),
        vec![formula(
            "root",
            &format!("{}/root", server.uri()),
            &digest,
            ":any_skip_relocation",
        )],
    );

    let error = install::run(&context, args(&["root"]))
        .await
        .expect_err("skeleton conflict");
    assert!(error.to_string().contains("skeleton destination"));
    assert!(!environment.prefix.join("etc/a.conf").exists());
    assert_eq!(
        std::fs::read(environment.prefix.join("etc/z")).expect("blocker preserved"),
        b"preexisting"
    );
    assert!(!environment.prefix.join("bin/root").exists());
    assert!(!environment.prefix.join("opt/root").exists());
    assert!(!environment.linked.join("root").exists());
    assert!(!environment.cellar.join("root").exists());
}

#[tokio::test]
async fn locks_are_taken_in_sorted_name_order_and_released_on_error() {
    let temp = TempDir::new().expect("temp");
    let environment = env(&temp);
    let higher = LockGuard::acquire(&environment.locks, "z.formula.lock").expect("higher lock");
    let context = ctx(
        environment.clone(),
        vec![
            formula("z", "http://unused/z", &"0".repeat(64), ":any"),
            formula("a", "http://unused/a", &"1".repeat(64), ":any"),
        ],
    );

    let error = install::run(&context, args(&["z", "a"]))
        .await
        .expect_err("higher lock busy");
    assert!(error.to_string().contains("z.formula.lock"));
    let lower = LockGuard::acquire(&environment.locks, "a.formula.lock")
        .expect("lower lock released after failed sorted acquisition");
    drop(lower);
    drop(higher);
    let z = LockGuard::acquire(&environment.locks, "z.formula.lock")
        .expect("higher lock released by owner");
    drop(z);
}

#[cfg(unix)]
#[tokio::test]
async fn cellar_symlink_is_refused_without_outside_mutation() {
    let server = MockServer::start().await;
    let bytes = tarball("root", &[("bin/root", b"root")]);
    let digest = sha(&bytes);
    mount(&server, "/root", &bytes).await;
    let temp = TempDir::new().expect("temp");
    let environment = env(&temp);
    let outside = Utf8PathBuf::from_path_buf(temp.path().join("outside")).expect("outside");
    std::fs::create_dir_all(&outside).expect("outside dir");
    std::fs::create_dir_all(&environment.prefix).expect("prefix");
    std::os::unix::fs::symlink(&outside, &environment.cellar).expect("cellar symlink");
    let context = ctx(
        environment.clone(),
        vec![formula(
            "root",
            &format!("{}/root", server.uri()),
            &digest,
            ":any_skip_relocation",
        )],
    );

    let error = install::run(&context, args(&["root"]))
        .await
        .expect_err("symlink refusal");
    assert!(error.to_string().contains("symlink") || error.to_string().contains("real directory"));
    assert_eq!(
        std::fs::read_dir(&outside).expect("outside read").count(),
        0
    );
}
