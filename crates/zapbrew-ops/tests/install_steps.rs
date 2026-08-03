use std::collections::HashMap;
use std::ffi::OsString;
use std::io;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::os::unix::process::ExitStatusExt;
use std::process::ExitStatus;
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
use zapbrew_ops::install::{self, Args};
use zapbrew_ops::{Ctx, OpError, Reporter};
use zapbrew_prefix::{CommandOutput, CommandRunner, CommandSpec, Env, EnvDetectInput};

#[derive(Default)]
struct RecordingReporter {
    warnings: Mutex<Vec<String>>,
}

impl Reporter for RecordingReporter {
    fn ohai(&self, _: &str) {}
    fn oh1(&self, _: &str) {}
    fn opoo(&self, message: &str) {
        self.warnings
            .lock()
            .expect("warnings lock")
            .push(message.to_owned());
    }
    fn onoe(&self, _: &str) {}
    fn print(&self, _: &str) {}
    fn eprint(&self, _: &str) {}
}

struct RecordingRunner {
    status: i32,
    specs: Mutex<Vec<CommandSpec>>,
}

impl RecordingRunner {
    fn successful() -> Self {
        Self {
            status: 0,
            specs: Mutex::new(Vec::new()),
        }
    }

    fn failing() -> Self {
        Self {
            status: 1,
            specs: Mutex::new(Vec::new()),
        }
    }

    fn specs(&self) -> Vec<CommandSpec> {
        self.specs.lock().expect("spec lock").clone()
    }
}

impl CommandRunner for RecordingRunner {
    fn run(&self, spec: &CommandSpec) -> Result<CommandOutput, io::Error> {
        self.specs.lock().expect("spec lock").push(spec.clone());
        let raw = if self.status == 0 {
            0
        } else {
            self.status << 8
        };
        Ok(CommandOutput::new(
            ExitStatus::from_raw(raw),
            Vec::new(),
            if self.status == 0 {
                Vec::new()
            } else {
                b"injected failure".to_vec()
            },
        ))
    }
}

struct OrderingRunner {
    before: Utf8PathBuf,
    after: Utf8PathBuf,
    observations: Mutex<Vec<(String, bool, bool)>>,
}

impl OrderingRunner {
    fn observations(&self) -> Vec<(String, bool, bool)> {
        self.observations.lock().expect("order lock").clone()
    }
}

impl CommandRunner for OrderingRunner {
    fn run(&self, spec: &CommandSpec) -> Result<CommandOutput, io::Error> {
        let program = std::path::Path::new(spec.program())
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .into_owned();
        self.observations.lock().expect("order lock").push((
            program,
            self.before.exists(),
            self.after.exists(),
        ));
        Ok(CommandOutput::new(
            ExitStatus::from_raw(0),
            Vec::new(),
            Vec::new(),
        ))
    }
}

struct PanicRunner;
impl CommandRunner for PanicRunner {
    fn run(&self, spec: &CommandSpec) -> Result<CommandOutput, io::Error> {
        panic!("host command must not run: {:?}", spec.program())
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
    .expect("env")
}

fn context(
    env: Env,
    formulae: Vec<Value>,
    runner: Arc<dyn CommandRunner>,
) -> (Ctx, Arc<RecordingReporter>) {
    let payload = serde_json::to_vec(&formulae).expect("json");
    let reporter = Arc::new(RecordingReporter::default());
    (
        Ctx {
            catalog: Arc::new(Catalog::from_payload(&payload, &env.bottle_tag).expect("catalog")),
            casks: Arc::new(CaskCatalog::from_payload(b"[]", &env.bottle_tag).expect("casks")),
            env,
            http: reqwest::Client::new(),
            commands: runner,
            reporter: reporter.clone(),
        },
        reporter,
    )
}

fn bottle(name: &str, entries: &[(&str, &[u8])]) -> Vec<u8> {
    let encoder = GzEncoder::new(Vec::new(), Compression::default());
    let mut archive = tar::Builder::new(encoder);
    for (relative, body) in entries {
        let mut header = tar::Header::new_gnu();
        header.set_size(body.len() as u64);
        header.set_mode(if relative.starts_with("bin/") {
            0o755
        } else {
            0o644
        });
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

fn formula(name: &str, url: &str, digest: &str, steps: Value) -> Value {
    json!({
        "name": name,
        "full_name": name,
        "versions": {"stable": "1.0", "bottle": true},
        "bottle": {"stable": {"rebuild": 0, "root_url": "unused", "files": {
            "x86_64_linux": {"cellar": ":any_skip_relocation", "url": url, "sha256": digest}
        }}},
        "post_install_steps": steps
    })
}

fn helper_formula(name: &str) -> Value {
    json!({
        "name": name,
        "full_name": name,
        "versions": {"stable": "1.0", "bottle": false}
    })
}

async fn mount(server: &MockServer, bytes: &[u8], expected: u64) {
    Mock::given(method("GET"))
        .and(path("/root"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(bytes.to_vec()))
        .expect(expected)
        .mount(server)
        .await;
}

fn args() -> Args {
    Args {
        names: vec!["root".to_owned()],
        ..Args::default()
    }
}

fn path_spec(base: &str, path: &str) -> Value {
    json!({"base": base, "path": path})
}

#[tokio::test]
async fn executes_all_generic_filesystem_steps_guards_tokens_and_commands() {
    let server = MockServer::start().await;
    let bytes = bottle(
        "root",
        &[
            ("bin/helper", b"recorded only"),
            ("share/source.txt", b"alpha alpha"),
            ("share/copy.txt", b"copy me"),
            ("share/children/child", b"child"),
            ("share/tree/a", b"linked"),
        ],
    );
    mount(&server, &bytes, 1).await;
    let digest = sha(&bytes);
    let steps = json!([
        {"type":"mkdir_p", "path":path_spec("var", "root/state")},
        {"type":"mkdir", "path":path_spec("var", "root/state/single")},
        {"type":"touch", "path":path_spec("var", "root/state/single/marker")},
        {"type":"mkdir_p", "path":path_spec("var", "root/guarded"), "guards":[{"condition":"on", "value":"linux"}]},
        {"type":"touch", "path":path_spec("var", "root/guarded/matched"), "guards":[{"condition":"if_exists", "base":"var", "path":"root/guarded"}]},
        {"type":"touch", "path":path_spec("var", "root/skipped"), "guards":[{"condition":"unless_exists", "base":"var", "path":"root/guarded"}]},
        {"type":"write", "path":path_spec("var", "root/state/config"), "content":"{{formula_name}}={{version}}@{{HOMEBREW_PREFIX}}\n", "overwrite":true},
        {"type":"copy", "source":path_spec("prefix", "share/copy.txt"), "target":path_spec("var", "root/state/copied"), "recursive":false, "overwrite":true},
        {"type":"move", "source":path_spec("prefix", "share/source.txt"), "target":path_spec("var", "root/state/moved"), "force":false, "overwrite":true, "source_glob":false},
        {"type":"move_contents", "source":path_spec("prefix", "share/children"), "target":path_spec("var", "root/state/moved-children")},
        {"type":"inreplace", "path":path_spec("var", "root/state/moved"), "before":"alpha", "after":"beta", "first_only":true},
        {"type":"link_dir", "source":path_spec("prefix", "share/tree"), "target":path_spec("homebrew_prefix", "share/tree-links")},
        {"type":"link_children", "source":path_spec("prefix", "share/tree"), "target":path_spec("homebrew_prefix", "share/child-links"), "prefix":"pre-", "suffix":"-suf"},
        {"type":"symlink", "source":path_spec("relative", "moved"), "target":path_spec("var", "root/state/link"), "force":false, "source_glob":false},
        {"type":"set_permissions", "paths":[path_spec("var", "root/state/moved")], "permissions":"0600", "non_recursive":true},
        {"type":"remove", "paths":[path_spec("var", "root/state/copied")], "recursive":false, "sudo":false},
        {"type":"warn", "message":"configured {{name}} {{version.major_minor}}"},
        {"type":"run", "command":path_spec("bin", "helper"), "args":["--name={{name}}"], "env":{"ROOT":"{{prefix}}"}, "sudo":false},
        {"type":"update_desktop_database", "path":path_spec("homebrew_prefix", "share/applications")}
    ]);
    let temp = TempDir::new().expect("temp");
    let environment = env(&temp);
    let runner = Arc::new(RecordingRunner::successful());
    let (ctx, reporter) = context(
        environment.clone(),
        vec![
            formula("root", &format!("{}/root", server.uri()), &digest, steps),
            helper_formula("desktop-file-utils"),
        ],
        runner.clone(),
    );

    install::run(&ctx, args())
        .await
        .expect("structured install");

    let state = environment.prefix.join("var/root/state");
    assert!(state.join("single/marker").is_file());
    assert!(
        environment
            .prefix
            .join("var/root/guarded/matched")
            .is_file()
    );
    assert!(!environment.prefix.join("var/root/skipped").exists());
    assert_eq!(
        std::fs::read_to_string(state.join("config")).expect("config"),
        format!("root=1.0@{}\n", environment.prefix)
    );
    assert!(!state.join("copied").exists());
    assert_eq!(
        std::fs::read_to_string(state.join("moved")).expect("moved"),
        "beta alpha"
    );
    assert_eq!(
        std::fs::read(state.join("moved-children/child")).expect("child"),
        b"child"
    );
    assert_eq!(
        std::fs::read_link(state.join("link")).expect("relative link"),
        std::path::PathBuf::from("moved")
    );
    assert!(environment.prefix.join("share/tree-links/a").is_symlink());
    assert!(
        environment
            .prefix
            .join("share/child-links/pre-a-suf")
            .is_symlink()
    );
    assert_eq!(
        std::fs::metadata(state.join("moved")).expect("mode").mode() & 0o777,
        0o600
    );
    assert!(
        reporter
            .warnings
            .lock()
            .expect("warnings")
            .contains(&"configured root 1.0".to_owned())
    );
    let specs = runner.specs();
    assert_eq!(specs.len(), 2);
    assert_eq!(
        specs[0].program(),
        environment
            .cellar
            .join("root/1.0/bin/helper")
            .as_std_path()
            .as_os_str()
    );
    assert_eq!(specs[0].arguments(), [OsString::from("--name=root")]);
    assert_eq!(
        specs[0].environment().get(&OsString::from("ROOT")),
        Some(&OsString::from(
            environment.cellar.join("root/1.0").as_str()
        ))
    );
    assert_eq!(
        specs[1].program(),
        environment
            .cellar
            .join("desktop-file-utils/1.0/bin/update-desktop-database")
            .as_std_path()
            .as_os_str()
    );
    assert_eq!(
        specs[1].arguments(),
        [OsString::from(
            environment.prefix.join("share/applications").as_str()
        )]
    );
}

#[tokio::test]
async fn malformed_steps_fail_before_lock_download_or_mutation() {
    let cases = [
        (json!([{"type":"special_magic"}]), "special_magic"),
        (
            json!([{"type":"touch", "path":{"base":"absolute", "path":"/tmp/x"}}]),
            "touch",
        ),
        (
            json!([{"type":"touch", "path":{"base":"var", "path":"../x"}}]),
            "touch",
        ),
        (
            json!([{"type":"touch", "path":{"base":"mystery", "path":"x"}}]),
            "touch",
        ),
        (
            json!([{"type":"touch", "path":{"base":"var", "path":"{{unknown}}"}}]),
            "touch",
        ),
        (
            json!([{"type":"touch", "path":{"base":"var", "path":"x"}, "guards":[{"condition":"sometimes"}]}]),
            "touch",
        ),
        (
            json!([{"type":"run", "command":{"base":"relative", "path":"sh"}}]),
            "run",
        ),
        (
            json!([{"type":"run", "command":{"base":"bin", "path":"x"}, "stdout_path":{"base":"var", "path":"out"}}]),
            "run",
        ),
        (
            json!([{"type":"init_data_dir", "path":{"base":"var", "path":"db"}}]),
            "init_data_dir",
        ),
    ];
    for (steps, type_name) in cases {
        let temp = TempDir::new().expect("temp");
        let environment = env(&temp);
        let prefix = environment.prefix.clone();
        let cache = environment.cache.clone();
        let formula = formula("root", "http://unused/root", &"0".repeat(64), steps);
        let (ctx, _) = context(environment, vec![formula], Arc::new(PanicRunner));

        let error = install::run(&ctx, args())
            .await
            .expect_err("preflight rejection");

        assert!(matches!(error, OpError::InstallStep { index: 0, .. }));
        assert!(error.to_string().contains(type_name));
        assert!(!prefix.exists(), "{type_name} mutated prefix");
        assert!(!cache.exists(), "{type_name} downloaded");
    }
}

#[tokio::test]
async fn symlink_ancestor_escape_is_rejected_before_outside_mutation() {
    let temp = TempDir::new().expect("temp");
    let environment = env(&temp);
    let outside = Utf8PathBuf::from_path_buf(temp.path().join("outside")).expect("outside");
    std::fs::create_dir_all(&outside).expect("outside");
    std::fs::create_dir_all(&environment.prefix).expect("prefix");
    symlink(&outside, environment.prefix.join("var")).expect("escaping var");
    let steps = json!([{"type":"touch", "path":path_spec("var", "escaped")}]);
    let formula = formula("root", "http://unused/root", &"0".repeat(64), steps);
    let (ctx, _) = context(environment, vec![formula], Arc::new(PanicRunner));

    let error = install::run(&ctx, args())
        .await
        .expect_err("escape rejection");

    assert!(matches!(error, OpError::InstallStep { index: 0, .. }));
    assert!(error.to_string().contains("symlink ancestor escapes"));
    assert_eq!(
        std::fs::read_dir(outside).expect("outside entries").count(),
        0
    );
}

#[tokio::test]
async fn command_failure_rolls_back_prior_filesystem_steps_and_new_keg() {
    let server = MockServer::start().await;
    let bytes = bottle("root", &[("bin/helper", b"not executed")]);
    mount(&server, &bytes, 1).await;
    let digest = sha(&bytes);
    let steps = json!([
        {"type":"write", "path":path_spec("var", "root/created"), "content":"created", "overwrite":true},
        {"type":"run", "command":path_spec("bin", "helper"), "args":[]}
    ]);
    let temp = TempDir::new().expect("temp");
    let environment = env(&temp);
    let runner = Arc::new(RecordingRunner::failing());
    let (ctx, _) = context(
        environment.clone(),
        vec![formula(
            "root",
            &format!("{}/root", server.uri()),
            &digest,
            steps,
        )],
        runner.clone(),
    );

    let error = install::run(&ctx, args())
        .await
        .expect_err("command failure");

    assert!(matches!(error, OpError::CommandFailed { .. }));
    assert_eq!(runner.specs().len(), 1);
    assert!(!environment.prefix.join("var/root/created").exists());
    assert!(!environment.cellar.join("root/1.0").exists());
    assert!(!environment.prefix.join("bin/helper").exists());
    assert!(!environment.prefix.join("opt/root").exists());
    assert!(!environment.linked.join("root").exists());
}

#[tokio::test]
async fn structured_steps_run_without_legacy_hook_then_exact_legacy_warning_is_emitted() {
    let server = MockServer::start().await;
    let bytes = bottle("root", &[("bin/root", b"root")]);
    mount(&server, &bytes, 1).await;
    let digest = sha(&bytes);
    let steps = json!([{"type":"touch", "path":path_spec("var", "root/structured")}]);
    let temp = TempDir::new().expect("temp");
    let environment = env(&temp);
    let mut root = formula("root", &format!("{}/root", server.uri()), &digest, steps);
    root["post_install_defined"] = json!(true);
    let (ctx, reporter) = context(environment.clone(), vec![root], Arc::new(PanicRunner));

    install::run(&ctx, args()).await.expect("install");

    assert!(environment.prefix.join("var/root/structured").is_file());
    assert!(reporter.warnings.lock().expect("warnings").contains(
        &"root defines post_install; run brew postinstall root to execute it.".to_owned()
    ));
}

#[tokio::test]
async fn failed_mkdir_does_not_journal_or_remove_preexisting_tree() {
    let server = MockServer::start().await;
    let bytes = bottle("root", &[("bin/root", b"root")]);
    mount(&server, &bytes, 1).await;
    let digest = sha(&bytes);
    let temp = TempDir::new().expect("temp");
    let environment = env(&temp);
    let shared = environment.prefix.join("var/shared");
    std::fs::create_dir_all(&shared).expect("shared tree");
    std::fs::write(shared.join("state"), b"preserve me").expect("shared bytes");
    std::fs::set_permissions(&shared, std::fs::Permissions::from_mode(0o750)).expect("shared mode");
    let original_mode = std::fs::metadata(&shared).expect("shared metadata").mode();
    let steps = json!([{"type":"mkdir", "path":path_spec("var", "shared")}]);
    let (ctx, _) = context(
        environment.clone(),
        vec![formula(
            "root",
            &format!("{}/root", server.uri()),
            &digest,
            steps,
        )],
        Arc::new(PanicRunner),
    );

    install::run(&ctx, args())
        .await
        .expect_err("existing mkdir must fail");

    assert_eq!(
        std::fs::read(shared.join("state")).expect("preserved bytes"),
        b"preserve me"
    );
    assert_eq!(
        std::fs::metadata(&shared)
            .expect("preserved metadata")
            .mode(),
        original_mode
    );
    assert!(!environment.cellar.join("root/1.0").exists());
}

#[tokio::test]
async fn run_steps_execute_inline_and_maintenance_runs_last() {
    let server = MockServer::start().await;
    let bytes = bottle("root", &[("bin/generic", b"generic")]);
    mount(&server, &bytes, 1).await;
    let digest = sha(&bytes);
    let temp = TempDir::new().expect("temp");
    let environment = env(&temp);
    let keg = environment.cellar.join("root/1.0");
    let runner = Arc::new(OrderingRunner {
        before: keg.join("before"),
        after: keg.join("after"),
        observations: Mutex::new(Vec::new()),
    });
    let steps = json!([
        {"type":"touch", "path":path_spec("prefix", "before")},
        {"type":"run", "command":path_spec("bin", "generic")},
        {"type":"touch", "path":path_spec("prefix", "after")},
        {"type":"gdk_pixbuf_query_loaders"}
    ]);
    let (ctx, _) = context(
        environment,
        vec![
            formula("root", &format!("{}/root", server.uri()), &digest, steps),
            helper_formula("gdk-pixbuf"),
        ],
        runner.clone(),
    );

    install::run(&ctx, args()).await.expect("ordered steps");

    assert_eq!(
        runner.observations(),
        [
            ("generic".to_owned(), true, false),
            ("gdk-pixbuf-query-loaders".to_owned(), true, true),
        ]
    );
}

#[tokio::test]
async fn keg_steps_support_cellar_outside_prefix() {
    let server = MockServer::start().await;
    let bytes = bottle("root", &[("bin/root", b"root")]);
    mount(&server, &bytes, 1).await;
    let digest = sha(&bytes);
    let temp = TempDir::new().expect("temp");
    let root = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 temp");
    let mut environment = env(&temp);
    environment.cellar = root.join("external-cellar");
    let steps =
        json!([{"type":"write", "path":path_spec("prefix", "marker"), "content":"inside keg"}]);
    let (ctx, _) = context(
        environment.clone(),
        vec![formula(
            "root",
            &format!("{}/root", server.uri()),
            &digest,
            steps,
        )],
        Arc::new(PanicRunner),
    );

    install::run(&ctx, args())
        .await
        .expect("external cellar install");

    assert_eq!(
        std::fs::read(environment.cellar.join("root/1.0/marker")).expect("keg marker"),
        b"inside keg"
    );
}

#[tokio::test]
async fn cellar_symlink_ancestor_escape_is_rejected_before_mutation() {
    let temp = TempDir::new().expect("temp");
    let root = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 temp");
    let mut environment = env(&temp);
    environment.cellar = root.join("external-cellar");
    let outside = root.join("outside");
    std::fs::create_dir_all(&environment.cellar).expect("cellar");
    std::fs::create_dir_all(&outside).expect("outside");
    symlink(&outside, environment.cellar.join("root")).expect("escaping rack");
    let steps = json!([{"type":"touch", "path":path_spec("prefix", "escaped")}]);
    let root_formula = formula("root", "http://unused/root", &"0".repeat(64), steps);
    let (ctx, _) = context(environment, vec![root_formula], Arc::new(PanicRunner));

    let error = install::run(&ctx, args())
        .await
        .expect_err("cellar escape rejection");

    assert!(matches!(error, OpError::InstallStep { index: 0, .. }));
    assert!(error.to_string().contains("symlink ancestor escapes"));
    assert_eq!(
        std::fs::read_dir(outside).expect("outside entries").count(),
        0
    );
}

#[tokio::test]
async fn committed_dependency_survives_later_requested_root_rollback() {
    let server = MockServer::start().await;
    let dep_bytes = bottle("dep", &[("bin/dep", b"dep")]);
    let root_bytes = bottle("root", &[("bin/fail", b"fail")]);
    Mock::given(method("GET"))
        .and(path("/dep"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(dep_bytes.clone()))
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/root"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(root_bytes.clone()))
        .expect(1)
        .mount(&server)
        .await;
    let temp = TempDir::new().expect("temp");
    let environment = env(&temp);
    let dep = formula(
        "dep",
        &format!("{}/dep", server.uri()),
        &sha(&dep_bytes),
        Value::Null,
    );
    let mut root = formula(
        "root",
        &format!("{}/root", server.uri()),
        &sha(&root_bytes),
        json!([{"type":"run", "command":path_spec("bin", "fail")}]),
    );
    root["dependencies"] = json!(["dep"]);
    let (ctx, _) = context(
        environment.clone(),
        vec![root, dep],
        Arc::new(RecordingRunner::failing()),
    );

    let error = install::run(&ctx, args())
        .await
        .expect_err("requested root failure");

    assert!(matches!(error, OpError::CommandFailed { .. }));
    assert!(environment.cellar.join("dep/1.0/bin/dep").is_file());
    assert!(environment.prefix.join("bin/dep").is_symlink());
    assert!(environment.prefix.join("opt/dep").is_symlink());
    assert!(environment.linked.join("dep").is_symlink());
    assert!(!environment.cellar.join("root").exists());
    assert!(!environment.prefix.join("bin/fail").exists());
    assert!(!environment.prefix.join("opt/root").exists());
    assert!(!environment.linked.join("root").exists());
}

#[tokio::test]
async fn rollback_removes_prefix_links_into_external_cellar() {
    let server = MockServer::start().await;
    let bytes = bottle(
        "root",
        &[("bin/fail", b"fail"), ("share/tree/entry", b"entry")],
    );
    mount(&server, &bytes, 1).await;
    let digest = sha(&bytes);
    let temp = TempDir::new().expect("temp");
    let root = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 temp");
    let mut environment = env(&temp);
    environment.cellar = root.join("external-cellar");
    let steps = json!([
        {
            "type":"link_dir",
            "source":path_spec("prefix", "share/tree"),
            "target":path_spec("homebrew_prefix", "share/tree-links")
        },
        {"type":"run", "command":path_spec("bin", "fail")}
    ]);
    let (ctx, _) = context(
        environment.clone(),
        vec![formula(
            "root",
            &format!("{}/root", server.uri()),
            &digest,
            steps,
        )],
        Arc::new(RecordingRunner::failing()),
    );

    let error = install::run(&ctx, args())
        .await
        .expect_err("command failure");

    assert!(matches!(error, OpError::CommandFailed { .. }));
    assert!(!environment.prefix.join("share/tree-links").exists());
    assert!(!environment.cellar.join("root").exists());
}
