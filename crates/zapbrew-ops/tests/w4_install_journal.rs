use std::collections::HashMap;
use std::io::Write;
use std::sync::Arc;
use std::time::Instant;

use camino::{Utf8Path, Utf8PathBuf};
use flate2::Compression;
use flate2::write::GzEncoder;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};
use zapbrew_api::{CaskCatalog, Catalog};
use zapbrew_ops::install::{self, Args};
use zapbrew_ops::{Ctx, Reporter};
use zapbrew_prefix::{Env, EnvDetectInput};

mod support;
use support::PanicRunner;

const FORMULA_NAME: &str = "w4-journal";
const VERSION: &str = "1.0";
const WORKLOAD_ITERATIONS: usize = 2048;
const RECORDED_ROOTS_DIRECTORY: &str = "zapbrew-w4-install-journal-roots";
const SENTINEL_BYTES: &[u8] = b"sentinel-before-install\n";
const REPLACEMENT_BYTES: &[u8] = b"replacement-from-install\n";

struct NullReporter;

impl Reporter for NullReporter {
    fn ohai(&self, _: &str) {}
    fn oh1(&self, _: &str) {}
    fn opoo(&self, _: &str) {}
    fn onoe(&self, _: &str) {}
    fn print(&self, _: &str) {}
    fn eprint(&self, _: &str) {}
}

fn recorded_root(test_name: &str) -> Utf8PathBuf {
    let temp = Utf8PathBuf::from_path_buf(std::env::temp_dir()).expect("utf8 temp directory");
    let parent = temp.join(RECORDED_ROOTS_DIRECTORY);
    std::fs::create_dir_all(&parent).expect("recorded roots directory");
    let root = parent.join(format!("{test_name}-{}", std::process::id()));
    std::fs::create_dir(&root).expect("fresh recorded root; run the external cleanup hook");
    root
}

fn environment(root: &Utf8Path) -> Env {
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
    .expect("workload environment")
}

fn context(env: Env, formula: Value, http: reqwest::Client) -> Ctx {
    let payload = serde_json::to_vec(&[formula]).expect("catalog payload");
    Ctx {
        catalog: Arc::new(Catalog::from_payload(&payload, &env.bottle_tag).expect("catalog")),
        casks: Arc::new(CaskCatalog::from_payload(b"[]", &env.bottle_tag).expect("casks")),
        env,
        http,
        commands: Arc::new(PanicRunner),
        reporter: Arc::new(NullReporter),
    }
}

fn bottle() -> Vec<u8> {
    let encoder = GzEncoder::new(Vec::new(), Compression::default());
    let mut archive = tar::Builder::new(encoder);
    for (relative, body) in [
        ("bin/w4-journal", b"w4 executable\n".as_slice()),
        (
            "share/relocated.txt",
            b"prefix=@@HOMEBREW_PREFIX@@\n".as_slice(),
        ),
    ] {
        let mut header = tar::Header::new_gnu();
        header.set_size(body.len() as u64);
        header.set_mode(if relative.starts_with("bin/") {
            0o755
        } else {
            0o644
        });
        header.set_cksum();
        archive
            .append_data(
                &mut header,
                format!("{FORMULA_NAME}/{VERSION}/{relative}"),
                body,
            )
            .expect("bottle entry");
    }
    archive
        .into_inner()
        .expect("tar finish")
        .finish()
        .expect("gzip finish")
}

fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn formula(url: &str, digest: &str, steps: Value) -> Value {
    json!({
        "name": FORMULA_NAME,
        "full_name": FORMULA_NAME,
        "versions": {"stable": VERSION, "bottle": true},
        "revision": 0,
        "bottle": {"stable": {
            "rebuild": 0,
            "root_url": "unused",
            "files": {"x86_64_linux": {
                "cellar": ":any",
                "url": url,
                "sha256": digest
            }}
        }},
        "post_install_steps": steps
    })
}

async fn mount_bottle(server: &MockServer, bytes: &[u8], expected: u64) {
    Mock::given(method("GET"))
        .and(path("/w4-journal"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(bytes.to_vec()))
        .expect(expected)
        .mount(server)
        .await;
}

fn install_args() -> Args {
    Args {
        names: vec![FORMULA_NAME.to_owned()],
        ..Args::default()
    }
}

fn sentinel_path(env: &Env) -> Utf8PathBuf {
    env.prefix.join("var/w4-journal/managed.conf")
}

fn assert_no_journal_root(env: &Env) {
    let rack = env.cellar.join(FORMULA_NAME);
    if !rack.exists() {
        return;
    }
    for entry in std::fs::read_dir(rack.as_std_path()).expect("read formula rack") {
        let name = entry.expect("rack entry").file_name();
        assert!(
            !name.to_string_lossy().starts_with(".zapbrew-step-journal"),
            "structured-step journal root remained in {rack}"
        );
    }
}

#[tokio::test]
#[ignore = "release workload; run the built test executable directly with --ignored --nocapture"]
async fn w4_install_journal_release_workload() {
    let server = MockServer::start().await;
    let bytes = bottle();
    mount_bottle(&server, &bytes, WORKLOAD_ITERATIONS as u64).await;
    let digest = digest(&bytes);
    let steps = json!([{
        "type": "write",
        "path": {"base": "var", "path": "w4-journal/managed.conf"},
        "content": "replacement-from-install\n",
        "overwrite": true
    }]);
    let recorded_root = recorded_root("release-workload");
    let url = format!("{}/w4-journal", server.uri());
    let catalog_entry = formula(&url, &digest, steps);
    let http = reqwest::Client::new();
    let mut installs = Vec::with_capacity(WORKLOAD_ITERATIONS);

    for iteration in 0..WORKLOAD_ITERATIONS {
        let env = environment(&recorded_root.join(format!("iteration-{iteration:04}")));
        let sentinel = sentinel_path(&env);
        std::fs::create_dir_all(sentinel.parent().expect("sentinel parent"))
            .expect("create sentinel parent");
        std::fs::write(&sentinel, SENTINEL_BYTES).expect("write sentinel");
        installs.push((context(env, catalog_entry.clone(), http.clone()), sentinel));
    }

    let started = Instant::now();
    for (ctx, _) in &installs {
        install::run(ctx, install_args())
            .await
            .expect("journal workload install");
    }
    let elapsed = started.elapsed();
    for (ctx, sentinel) in &installs {
        assert_eq!(
            std::fs::read(sentinel).expect("replacement bytes"),
            REPLACEMENT_BYTES
        );
        let keg = ctx.env.cellar.join(FORMULA_NAME).join(VERSION);
        assert!(keg.join("bin/w4-journal").is_file());
        assert!(keg.join("INSTALL_RECEIPT.json").is_file());
        assert_eq!(
            std::fs::read_to_string(keg.join("share/relocated.txt")).expect("relocated file"),
            format!("prefix={}\n", ctx.env.prefix)
        );
        assert!(ctx.env.prefix.join("bin/w4-journal").is_symlink());
        assert!(ctx.env.prefix.join("opt/w4-journal").is_symlink());
        assert!(ctx.env.linked.join(FORMULA_NAME).is_symlink());
        assert_no_journal_root(&ctx.env);
    }

    let stdout = std::io::stdout();
    let mut output = stdout.lock();
    writeln!(
        output,
        "W4_INSTALL_JOURNAL iterations={} install_loop_elapsed_ns={} install_loop_elapsed_seconds={:.9}",
        WORKLOAD_ITERATIONS,
        elapsed.as_nanos(),
        elapsed.as_secs_f64()
    )
    .expect("write workload measurement");
}

#[tokio::test]
#[ignore = "deterministic rollback workload"]
async fn w4_install_journal_restores_overwrite_after_later_failure() {
    let server = MockServer::start().await;
    let bytes = bottle();
    mount_bottle(&server, &bytes, 1).await;
    let digest = digest(&bytes);
    let steps = json!([
        {
            "type": "write",
            "path": {"base": "var", "path": "w4-journal/managed.conf"},
            "content": "replacement-from-install\n",
            "overwrite": true
        },
        {
            "type": "mkdir",
            "path": {"base": "var", "path": "w4-journal/already-exists"}
        }
    ]);
    let recorded_root = recorded_root("rollback");
    let env = environment(&recorded_root);
    let sentinel = sentinel_path(&env);
    std::fs::create_dir_all(sentinel.parent().expect("sentinel parent"))
        .expect("create sentinel parent");
    std::fs::write(&sentinel, SENTINEL_BYTES).expect("write sentinel");
    std::fs::create_dir(env.prefix.join("var/w4-journal/already-exists"))
        .expect("create later-step blocker");
    let formula = formula(&format!("{}/w4-journal", server.uri()), &digest, steps);
    let ctx = context(env, formula, reqwest::Client::new());

    install::run(&ctx, install_args())
        .await
        .expect_err("later structured step must fail");

    assert_eq!(
        std::fs::read(&sentinel).expect("restored sentinel"),
        SENTINEL_BYTES
    );
    assert!(!ctx.env.cellar.join(FORMULA_NAME).join(VERSION).exists());
    assert_no_journal_root(&ctx.env);
}
