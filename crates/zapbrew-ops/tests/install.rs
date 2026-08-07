use std::collections::HashMap;
use std::io;
use std::os::unix::fs::symlink;
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
use zapbrew_ops::{Ctx, Reporter};
use zapbrew_prefix::{CommandOutput, CommandRunner, CommandSpec, Env, EnvDetectInput, Tab};

const SIGNED_MIGRATIONS: &str = r#"{"payload":"{\"android-ndk\":\"homebrew/cask\",\"android-platform-tools\":\"homebrew/cask\",\"app-engine-go-32\":\"homebrew/cask/google-cloud-sdk\",\"app-engine-go-64\":\"homebrew/cask/google-cloud-sdk\",\"avidemux\":\"homebrew/cask\",\"chromedriver\":\"homebrew/cask\",\"cockatrice\":\"homebrew/cask\",\"codex\":\"homebrew/cask\",\"consul\":\"homebrew/cask\",\"copilot-language-server\":\"homebrew/cask\",\"corelocationcli\":\"homebrew/cask\",\"geany\":\"homebrew/cask\",\"gearboy\":\"homebrew/cask\",\"gearsystem\":\"homebrew/cask\",\"gimp\":\"homebrew/cask\",\"grads\":\"homebrew/cask\",\"gtkwave\":\"homebrew/cask\",\"inkscape\":\"homebrew/cask\",\"joplin\":\"homebrew/cask\",\"keybase\":\"homebrew/cask\",\"luanti\":\"homebrew/cask\",\"meld\":\"homebrew/cask\",\"minetest\":\"homebrew/cask/luanti\",\"mitmproxy\":\"homebrew/cask\",\"openrct2\":\"homebrew/cask\",\"openttd\":\"homebrew/cask\",\"osxfuse\":\"homebrew/cask\",\"quassel\":\"homebrew/cask\",\"schismtracker\":\"homebrew/cask/schism-tracker\",\"transmission-remote-gtk\":\"homebrew/cask/transmission-remote-gui\",\"truetree\":\"homebrew/cask\",\"wesnoth\":\"homebrew/cask/the-battle-for-wesnoth\"}","signatures":[{"protected":"eyJhbGciOiJQUzUxMiIsImI2NCI6ZmFsc2UsImNyaXQiOlsiYjY0Il19","header":{"kid":"homebrew-1"},"signature":"l89BOBTAX2oo91_KlSCxmHHXt8jDj2dx0AssxCgC1wT-RBc8rL4alNd9XIUR8hJ5Yw7WLlWIgj40ktanmdBEUG_HYi_ll7FLX_99tRXky8Rhvzdl7XDjX9ixE3ICvN-7sIVB7qZ809g4GTawj01g6yozQvr0OGyuo_haqWYunGPqbuamOB-W0L4h-uyUySLH8nsrxI9PT-mOquTcnopyKAVqoQ_SOZ1_f6l0Djoph40UumxpSSGD04lQHT5xtYvkkNeudlPB6kc3wHzl1iBFRDCpWOnoR6-qTgqJzOhyDFUapLgAbUSF3bEmOoVuYaxZ5U5s1Hr5lUl7sHXJsL1A0xlwIOFJFutyAnPHhgG5urJviJcmVmBxaak1g_FwDF6yopvEJFdSWYOgmPPP8Fiwp4-Z9izNFBTBLmYcfj491LpOwifvxt0ZdWcsY8hllSBkKcyD3TO02Bz7IjlOHLqoDTfMae94qZUObTlknbM5Z7w3fHxl91MVqmFUxSeaRkZlm-BDf_wCcvbetxWlfbMfKijnWHoOub8lRH4-ywTLBPYaTWnJWwGwUSBN5nUkFtaXFDVQqfRPZnWQO9wjBu8iv67VcT7WnpfN9MvO-dvptEoMx_NYfPxXCbk90Tpmlrpoq5QZOEmUz9UK_3-ur3CRdUwUsVNVcGiLwJ-8kNn72JU"},{"protected":"eyJhbGciOiJQUzUxMiIsImI2NCI6ZmFsc2V9","header":{"kid":"zapbrew-test"},"signature":"iuTpZBrHtAfRmSiKGiC3C4Otw8uFRRsxj3fFz76iY0LarfdIQi1baatHAj4f48FE3kGxWRzMYdaSSXkkQiwh0uG5VdY1ZcIZ6ZyX80-GpcQ1jdano8YkTIXZnRVVADpaM-bCF80Abl1_ZsvnaLFQkYHEUv-1M1hcNpmyiPqj0T4LIM926PLefrfPaMhmDhhyusB179or0S_1nvL59FZiyHejVnjna2dyu0wQ7Gymqya-CQP02_rw2I7fUVtdzG7WYrqLL1KcKxXpMA06Y3CCRziftEZnmsxVjYezEcMcl_FBB_zjn1QAbR1VKuD5UDnp1NcLTYUYKgRR5oGPE6P5DA"}]}"#;

struct PanicRunner;

impl CommandRunner for PanicRunner {
    fn run(&self, spec: &CommandSpec) -> Result<CommandOutput, io::Error> {
        panic!("host command must not run: {:?}", spec.program())
    }
}

#[derive(Default)]
struct RecordingReporter {
    messages: Mutex<Vec<String>>,
    quiet: bool,
    verbose: bool,
}

impl RecordingReporter {
    fn record(&self, channel: &str, message: &str) {
        self.messages
            .lock()
            .expect("reporter lock")
            .push(format!("{channel}:{message}"));
    }

    fn take(&self) -> Vec<String> {
        std::mem::take(&mut *self.messages.lock().expect("reporter lock"))
    }
}

impl Reporter for RecordingReporter {
    fn ohai(&self, message: &str) {
        self.record("ohai", message);
    }
    fn oh1(&self, message: &str) {
        self.record("oh1", message);
    }
    fn opoo(&self, message: &str) {
        self.record("opoo", message);
    }
    fn onoe(&self, message: &str) {
        self.record("onoe", message);
    }
    fn print(&self, message: &str) {
        self.record("print", message);
    }
    fn eprint(&self, message: &str) {
        self.record("eprint", message);
    }
    fn is_quiet(&self) -> bool {
        self.quiet
    }
    fn is_verbose(&self) -> bool {
        self.verbose
    }
}

fn scratch_env(temp: &TempDir) -> Env {
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
    context_flags(env, formulae, RecordingReporter::default())
}

fn context_flags(
    env: Env,
    formulae: Vec<Value>,
    recording: RecordingReporter,
) -> (Ctx, Arc<RecordingReporter>) {
    let payload = serde_json::to_vec(&formulae).expect("catalog json");
    let catalog = Arc::new(Catalog::from_payload(&payload, &env.bottle_tag).expect("catalog"));
    let casks = Arc::new(CaskCatalog::from_payload(b"[]", &env.bottle_tag).expect("casks"));
    let recording = Arc::new(recording);
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

fn bottle(name: &str, version: &str, files: &[(&str, &[u8])]) -> Vec<u8> {
    let encoder = GzEncoder::new(Vec::new(), Compression::default());
    let mut archive = tar::Builder::new(encoder);
    for (relative, body) in files {
        let mut header = tar::Header::new_gnu();
        header.set_size(body.len() as u64);
        header.set_mode(if relative.starts_with("bin/") {
            0o755
        } else {
            0o644
        });
        header.set_cksum();
        archive
            .append_data(&mut header, format!("{name}/{version}/{relative}"), *body)
            .expect("tar entry");
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

fn formula(name: &str, version: &str, url: &str, sha: &str) -> Value {
    json!({
        "name": name,
        "full_name": name,
        "versions": {"stable": version, "bottle": true},
        "revision": 0,
        "bottle": {"stable": {
            "rebuild": 0,
            "root_url": "unused",
            "files": {"x86_64_linux": {
                "cellar": ":any_skip_relocation",
                "url": url,
                "sha256": sha
            }}
        }}
    })
}

async fn mount_blob(server: &MockServer, route: &str, bytes: &[u8], expected: u64) {
    Mock::given(method("GET"))
        .and(path(route))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(bytes.to_vec()))
        .expect(expected)
        .mount(server)
        .await;
}

fn args(name: &str) -> Args {
    Args {
        names: vec![name.to_owned()],
        ..Args::default()
    }
}

#[tokio::test]
async fn installs_real_bottle_with_tab_links_skeleton_caveats_and_summary() {
    let server = MockServer::start().await;
    let tarball = bottle(
        "root",
        "1.0",
        &[
            ("bin/root", b"root executable"),
            (".bottle/etc/root.conf", b"fresh"),
        ],
    );
    let sha = digest(&tarball);
    mount_blob(&server, "/root", &tarball, 1).await;
    let temp = TempDir::new().expect("temp");
    let env = scratch_env(&temp);
    let mut root = formula("root", "1.0", &format!("{}/root", server.uri()), &sha);
    root["aliases"] = json!(["r"]);
    root["ruby_source_path"] = json!("Formula/r/root.rb");
    root["tap_git_head"] = json!("abc123");
    root["caveats"] = json!("Config lives in $HOMEBREW_PREFIX/etc.");
    let (ctx, reporter) = context(env, vec![root]);

    install::run(&ctx, args("root")).await.expect("install");

    let keg = ctx.env.cellar.join("root/1.0");
    assert!(keg.join("bin/root").is_file());
    assert_eq!(
        std::fs::read(ctx.env.prefix.join("etc/root.conf")).expect("skeleton"),
        b"fresh"
    );
    assert!(ctx.env.prefix.join("bin/root").is_symlink());
    assert!(ctx.env.prefix.join("opt/root").is_symlink());
    assert!(ctx.env.linked.join("root").is_symlink());
    let tab = Tab::load(keg.join("INSTALL_RECEIPT.json")).expect("tab");
    assert!(tab.installed_on_request);
    assert!(tab.poured_from_bottle);
    assert!(tab.loaded_from_api);
    assert!(tab.loaded_from_internal_api);
    assert_eq!(tab.aliases, ["r"]);
    assert_eq!(tab.source.path.as_deref(), Some("Formula/r/root.rb"));
    assert_eq!(tab.source.tap_git_head.as_deref(), Some("abc123"));
    assert_eq!(tab.source.versions.stable.as_deref(), Some("1.0"));
    let receipt = std::fs::read_to_string(keg.join("INSTALL_RECEIPT.json")).expect("receipt");
    assert!(receipt.ends_with('\n'));
    assert!(receipt.contains("\n  \"used_options\""));

    let messages = reporter.take();
    assert_eq!(
        &messages[..3],
        vec![
            "ohai:Fetching root".to_owned(),
            "ohai:Pouring root--1.0.x86_64_linux.bottle.tar.gz".to_owned(),
            "ohai:Caveats".to_owned(),
        ]
    );
    assert_eq!(
        messages[3],
        format!("print:Config lives in {}/etc.", ctx.env.prefix)
    );
    assert!(messages[4].starts_with(&format!("print:🍺  {keg}: ")));
}

#[tokio::test]
async fn verbose_emits_download_url_and_quiet_drops_caveats() {
    let server = MockServer::start().await;
    let tarball = bottle("root", "1.0", &[("bin/root", b"root")]);
    let sha = digest(&tarball);
    mount_blob(&server, "/root", &tarball, 2).await;
    let verbose_temp = TempDir::new().expect("temp");
    let quiet_temp = TempDir::new().expect("temp");
    let mut root = formula("root", "1.0", &format!("{}/root", server.uri()), &sha);
    root["caveats"] = json!("Config lives in $HOMEBREW_PREFIX/etc.");

    let (verbose_ctx, verbose) = context_flags(
        scratch_env(&verbose_temp),
        vec![root.clone()],
        RecordingReporter {
            verbose: true,
            ..RecordingReporter::default()
        },
    );
    install::run(&verbose_ctx, args("root"))
        .await
        .expect("install");
    let verbose_messages = verbose.take();
    assert!(verbose_messages.contains(&format!("oh1:Downloading {}/root", server.uri())));
    assert!(verbose_messages.iter().any(|line| line == "ohai:Caveats"));

    let (quiet_ctx, quiet) = context_flags(
        scratch_env(&quiet_temp),
        vec![root],
        RecordingReporter {
            quiet: true,
            ..RecordingReporter::default()
        },
    );
    install::run(&quiet_ctx, args("root"))
        .await
        .expect("install");
    let quiet_messages = quiet.take();
    assert!(
        !quiet_messages
            .iter()
            .any(|line| line.starts_with("oh1:Downloading"))
    );
    assert!(!quiet_messages.iter().any(|line| line == "ohai:Caveats"));
    assert!(
        !quiet_messages
            .iter()
            .any(|line| line.starts_with("print:Config lives in"))
    );
}

#[tokio::test]
async fn installs_dependencies_postorder_and_marks_graph_slice() {
    let server = MockServer::start().await;
    let dep_tar = bottle("dep", "2.0", &[("bin/dep", b"dep")]);
    let root_tar = bottle("root", "1.0", &[("bin/root", b"root")]);
    let dep_sha = digest(&dep_tar);
    let root_sha = digest(&root_tar);
    mount_blob(&server, "/dep", &dep_tar, 1).await;
    mount_blob(&server, "/root", &root_tar, 1).await;
    let temp = TempDir::new().expect("temp");
    let env = scratch_env(&temp);
    let dep = formula("dep", "2.0", &format!("{}/dep", server.uri()), &dep_sha);
    let mut root = formula("root", "1.0", &format!("{}/root", server.uri()), &root_sha);
    root["dependencies"] = json!(["dep"]);
    let (ctx, reporter) = context(env, vec![root, dep]);

    install::run(&ctx, args("root")).await.expect("install");

    let dep_tab = Tab::load(ctx.env.cellar.join("dep/2.0/INSTALL_RECEIPT.json")).expect("dep tab");
    assert!(!dep_tab.installed_on_request);
    let root_tab =
        Tab::load(ctx.env.cellar.join("root/1.0/INSTALL_RECEIPT.json")).expect("root tab");
    let runtime = root_tab.runtime_dependencies.expect("runtime dependencies");
    assert_eq!(runtime.len(), 1);
    assert_eq!(runtime[0].full_name, "dep");
    assert_eq!(runtime[0].version, "2.0");
    assert_eq!(runtime[0].pkg_version, "2.0");
    assert!(runtime[0].declared_directly);
    let messages = reporter.take();
    let pours: Vec<_> = messages
        .iter()
        .filter(|message| message.starts_with("ohai:Pouring"))
        .cloned()
        .collect();
    assert_eq!(
        pours,
        [
            "ohai:Pouring dep--2.0.x86_64_linux.bottle.tar.gz",
            "ohai:Pouring root--1.0.x86_64_linux.bottle.tar.gz"
        ]
    );
}

#[tokio::test]
async fn already_current_warns_without_fetch_and_force_replaces() {
    let server = MockServer::start().await;
    let tarball = bottle("root", "1.0", &[("bin/root", b"root")]);
    let sha = digest(&tarball);
    mount_blob(&server, "/root", &tarball, 1).await;
    let temp = TempDir::new().expect("temp");
    let env = scratch_env(&temp);
    let (ctx, reporter) = context(
        env,
        vec![formula(
            "root",
            "1.0",
            &format!("{}/root", server.uri()),
            &sha,
        )],
    );
    install::run(&ctx, args("root"))
        .await
        .expect("first install");
    reporter.take();

    install::run(&ctx, args("root"))
        .await
        .expect("already installed");
    assert_eq!(
        reporter.take(),
        [
            "opoo:root 1.0 is already installed and up-to-date.\nTo reinstall 1.0, run:\n  zapbrew reinstall root"
        ]
    );

    install::run(
        &ctx,
        Args {
            force: true,
            ..args("root")
        },
    )
    .await
    .expect("forced replacement");
    assert!(ctx.env.cellar.join("root/1.0/bin/root").is_file());
    assert!(ctx.env.prefix.join("bin/root").is_symlink());
}

#[tokio::test]
async fn only_dependencies_skips_root() {
    let server = MockServer::start().await;
    let dep_tar = bottle("dep", "1.0", &[("bin/dep", b"dep")]);
    let dep_sha = digest(&dep_tar);
    mount_blob(&server, "/dep", &dep_tar, 1).await;
    let temp = TempDir::new().expect("temp");
    let env = scratch_env(&temp);
    let dep = formula("dep", "1.0", &format!("{}/dep", server.uri()), &dep_sha);
    let mut root = formula(
        "root",
        "1.0",
        &format!("{}/unused", server.uri()),
        &"0".repeat(64),
    );
    root["dependencies"] = json!(["dep"]);
    let (ctx, _) = context(env, vec![root, dep]);

    install::run(
        &ctx,
        Args {
            only_dependencies: true,
            ..args("root")
        },
    )
    .await
    .expect("dependencies only");

    assert!(ctx.env.cellar.join("dep/1.0").is_dir());
    assert!(!ctx.env.cellar.join("root").exists());
}

#[tokio::test]
async fn dry_run_has_zero_http_and_filesystem_mutation() {
    let server = MockServer::start().await;
    let tarball = bottle("root", "1.0", &[("bin/root", b"root")]);
    let sha = digest(&tarball);
    mount_blob(&server, "/root", &tarball, 0).await;
    let temp = TempDir::new().expect("temp");
    let env = scratch_env(&temp);
    let prefix = env.prefix.clone();
    let cache = env.cache.clone();
    let (ctx, reporter) = context(
        env,
        vec![formula(
            "root",
            "1.0",
            &format!("{}/root", server.uri()),
            &sha,
        )],
    );

    install::run(
        &ctx,
        Args {
            dry_run: true,
            ..args("root")
        },
    )
    .await
    .expect("dry run");

    assert!(!prefix.exists());
    assert!(!cache.exists());
    assert_eq!(reporter.take(), ["ohai:Would install", "print:root"]);
}

#[tokio::test]
async fn refuses_ruby_modes_before_catalog_network_or_filesystem() {
    for (field, expected) in [
        (
            "build",
            "zapbrew cannot build from source: formulae are Ruby definitions. Use bottles (default) or brew.",
        ),
        (
            "head",
            "zapbrew cannot install HEAD formulae: formulae are Ruby definitions. Use bottled stable releases or brew.",
        ),
        (
            "interactive",
            "zapbrew cannot install interactively: formulae are Ruby definitions. Use bottles (default) or brew.",
        ),
    ] {
        let temp = TempDir::new().expect("temp");
        let env = scratch_env(&temp);
        let prefix = env.prefix.clone();
        let (ctx, _) = context(env, Vec::new());
        let mut request = args("missing");
        request.build_from_source = field == "build";
        request.head = field == "head";
        request.interactive = field == "interactive";
        let error = install::run(&ctx, request).await.expect_err("refusal");
        assert_eq!(error.to_string(), expected);
        assert!(!prefix.exists());
    }
}

#[tokio::test]
async fn disabled_refuses_and_deprecated_warns() {
    let temp = TempDir::new().expect("temp");
    let env = scratch_env(&temp);
    let mut disabled = formula("disabled", "1", "http://unused", &"0".repeat(64));
    disabled["disabled"] = json!(true);
    disabled["disable_reason"] = json!("does not build");
    let (ctx, _) = context(env, vec![disabled]);
    let error = install::run(&ctx, args("disabled"))
        .await
        .expect_err("disabled refusal");
    assert_eq!(
        error.to_string(),
        "disabled has been disabled because it does not build!"
    );

    let server = MockServer::start().await;
    let tarball = bottle("old", "1", &[("bin/old", b"old")]);
    let sha = digest(&tarball);
    mount_blob(&server, "/old", &tarball, 1).await;
    let temp = TempDir::new().expect("temp");
    let env = scratch_env(&temp);
    let mut deprecated = formula("old", "1", &format!("{}/old", server.uri()), &sha);
    deprecated["deprecated"] = json!(true);
    deprecated["deprecation_reason"] = json!("is unmaintained");
    let (ctx, reporter) = context(env, vec![deprecated]);
    install::run(&ctx, args("old"))
        .await
        .expect("deprecated installs");
    assert!(
        reporter
            .take()
            .contains(&"opoo:old has been deprecated because it is unmaintained!".to_owned())
    );
}

#[tokio::test]
async fn keg_only_keeps_records_without_file_links() {
    let server = MockServer::start().await;
    let tarball = bottle("solo", "1", &[("bin/solo", b"solo")]);
    let sha = digest(&tarball);
    mount_blob(&server, "/solo", &tarball, 1).await;
    let temp = TempDir::new().expect("temp");
    let env = scratch_env(&temp);
    let mut solo = formula("solo", "1", &format!("{}/solo", server.uri()), &sha);
    solo["keg_only"] = json!(true);
    solo["keg_only_reason"] =
        json!({"reason": ":versioned_formula", "explanation": "it is versioned"});
    let (ctx, reporter) = context(env, vec![solo]);

    install::run(&ctx, args("solo")).await.expect("keg-only");

    assert!(!ctx.env.prefix.join("bin/solo").exists());
    assert!(ctx.env.prefix.join("opt/solo").is_symlink());
    assert!(ctx.env.linked.join("solo").is_symlink());
    assert!(reporter.take().contains(&format!(
        "opoo:solo is keg-only, which means it was not symlinked into {},\nbecause it is versioned.",
        ctx.env.prefix
    )));
}

#[tokio::test]
async fn pinned_outdated_formula_is_refused_before_fetch() {
    let server = MockServer::start().await;
    let old_bottle = bottle("root", "0.9", &[("bin/root", b"old")]);
    mount_blob(&server, "/old", &old_bottle, 1).await;
    let temp = TempDir::new().expect("temp");
    let environment = scratch_env(&temp);
    let (old_ctx, _) = context(
        environment.clone(),
        vec![formula(
            "root",
            "0.9",
            &format!("{}/old", server.uri()),
            &digest(&old_bottle),
        )],
    );
    install::run(&old_ctx, args("root"))
        .await
        .expect("old install");
    std::fs::create_dir_all(&environment.pins).expect("pins");
    symlink("../../../Cellar/root/0.9", environment.pins.join("root")).expect("pin");
    let (ctx, _) = context(
        environment,
        vec![formula("root", "1.0", "http://unused/new", &"0".repeat(64))],
    );

    let error = install::run(&ctx, args("root"))
        .await
        .expect_err("pinned outdated refusal");

    assert_eq!(
        error.to_string(),
        "root is pinned at 0.9 but 1.0 is available."
    );
}

#[tokio::test]
async fn installed_conflict_refusal_has_complete_guidance() {
    let server = MockServer::start().await;
    let blocker_bottle = bottle("blocker", "1.0", &[("bin/blocker", b"blocker")]);
    mount_blob(&server, "/blocker", &blocker_bottle, 1).await;
    let temp = TempDir::new().expect("temp");
    let environment = scratch_env(&temp);
    let blocker = formula(
        "blocker",
        "1.0",
        &format!("{}/blocker", server.uri()),
        &digest(&blocker_bottle),
    );
    let (blocker_ctx, _) = context(environment.clone(), vec![blocker.clone()]);
    install::run(&blocker_ctx, args("blocker"))
        .await
        .expect("blocker install");
    let mut root = formula("root", "1.0", "http://unused/root", &"0".repeat(64));
    root["conflicts_with"] = json!(["blocker"]);
    root["conflicts_with_reasons"] = json!(["both provide tool"]);
    let (ctx, _) = context(environment.clone(), vec![root, blocker]);

    let error = install::run(&ctx, args("root"))
        .await
        .expect_err("installed conflict");

    assert_eq!(
        error.to_string(),
        format!(
            "Cannot install root because conflicting formulae are installed.\n  blocker: because both provide tool\n\nPlease `brew unlink blocker` before continuing.\n\nUnlinking removes a formula's symlinks from {}. You can\nlink the formula again after the install finishes. You can `--force` this\ninstall, but the build may fail or cause obscure side effects in the\nresulting software.",
            environment.prefix
        )
    );
}

#[tokio::test]
async fn migrated_formula_refusal_surfaces_verified_tap_hint() {
    let server = MockServer::start().await;
    mount_blob(
        &server,
        "/formula_tap_migrations.jws.json",
        SIGNED_MIGRATIONS.as_bytes(),
        1,
    )
    .await;
    let temp = TempDir::new().expect("temp");
    let mut environment = scratch_env(&temp);
    environment.api_domain = server.uri();
    let (ctx, _) = context(environment, Vec::new());

    let error = install::run(&ctx, args("codex"))
        .await
        .expect_err("migration refusal");

    assert_eq!(error.to_string(), "codex was migrated to homebrew/cask");
}

#[tokio::test]
async fn missing_formula_has_exact_typed_message() {
    let server = MockServer::start().await;
    mount_blob(
        &server,
        "/formula_tap_migrations.jws.json",
        SIGNED_MIGRATIONS.as_bytes(),
        1,
    )
    .await;
    let temp = TempDir::new().expect("temp");
    let mut environment = scratch_env(&temp);
    environment.api_domain = server.uri();
    let (ctx, _) = context(environment, Vec::new());

    let error = install::run(&ctx, args("never-was"))
        .await
        .expect_err("missing formula");

    assert_eq!(
        error.to_string(),
        "No available formula with the name \"never-was\"."
    );
}

#[tokio::test]
async fn missing_host_bottle_is_refused_with_host_tag() {
    let temp = TempDir::new().expect("temp");
    let environment = scratch_env(&temp);
    let mut root = formula("root", "1.0", "http://unused/root", &"0".repeat(64));
    root["bottle"]["stable"]["files"] = json!({
        "arm64_linux": {
            "cellar": ":any_skip_relocation",
            "url": "http://unused/root",
            "sha256": "0".repeat(64)
        }
    });
    let (ctx, _) = context(environment, vec![root]);

    let error = install::run(&ctx, args("root"))
        .await
        .expect_err("host bottle refusal");

    assert_eq!(
        error.to_string(),
        "root: no bottle available for x86_64_linux. brew can build from source; zapbrew cannot."
    );
}

#[tokio::test]
async fn include_test_pours_test_dependencies_but_tab_excludes_them() {
    // dependency_candidates uses EdgeFilter::query(false, include_test, false, false)
    // make_tab uses EdgeFilter::default() and must never include build/test in runtime_dependencies
    let server = MockServer::start().await;
    let testdep_tar = bottle("testdep", "1.0", &[("bin/testdep", b"testdep")]);
    let builddep_tar = bottle("builddep", "1.0", &[("bin/builddep", b"builddep")]);
    let root_tar = bottle("root", "1.0", &[("bin/root", b"root")]);
    let testdep_sha = digest(&testdep_tar);
    let builddep_sha = digest(&builddep_tar);
    let root_sha = digest(&root_tar);
    mount_blob(&server, "/testdep", &testdep_tar, 1).await;
    mount_blob(&server, "/root", &root_tar, 2).await;
    let testdep = formula(
        "testdep",
        "1.0",
        &format!("{}/testdep", server.uri()),
        &testdep_sha,
    );
    let builddep = formula(
        "builddep",
        "1.0",
        &format!("{}/builddep", server.uri()),
        &builddep_sha,
    );
    let mut root = formula("root", "1.0", &format!("{}/root", server.uri()), &root_sha);
    root["test_dependencies"] = json!(["testdep"]);
    root["build_dependencies"] = json!(["builddep"]);

    // include_test = false: testdep must NOT be poured, builddep never poured
    let temp_false = TempDir::new().expect("temp");
    let env_false = scratch_env(&temp_false);
    let (ctx_false, _) = context(
        env_false.clone(),
        vec![root.clone(), testdep.clone(), builddep.clone()],
    );
    install::run(
        &ctx_false,
        Args {
            include_test: false,
            ..args("root")
        },
    )
    .await
    .expect("install without test");
    assert!(
        !ctx_false.env.cellar.join("testdep/1.0").exists(),
        "testdep should not be poured without include_test"
    );
    assert!(
        !ctx_false.env.cellar.join("builddep/1.0").exists(),
        "builddep should never be poured"
    );
    let root_tab_false =
        Tab::load(ctx_false.env.cellar.join("root/1.0/INSTALL_RECEIPT.json")).expect("tab");
    assert!(
        root_tab_false
            .runtime_dependencies
            .as_ref()
            .is_none_or(|deps| deps.is_empty()),
        "runtime_dependencies must exclude build/test deps"
    );

    // include_test = true: testdep must be poured, builddep still not, but tab still excludes both
    let temp_true = TempDir::new().expect("temp");
    let env_true = scratch_env(&temp_true);
    let (ctx_true, _) = context(env_true.clone(), vec![root, testdep, builddep]);
    install::run(
        &ctx_true,
        Args {
            include_test: true,
            ..args("root")
        },
    )
    .await
    .expect("install with test");
    assert!(
        ctx_true.env.cellar.join("testdep/1.0").exists(),
        "testdep should be poured with include_test"
    );
    assert!(
        !ctx_true.env.cellar.join("builddep/1.0").exists(),
        "builddep should never be poured even with include_test"
    );
    let root_tab_true =
        Tab::load(ctx_true.env.cellar.join("root/1.0/INSTALL_RECEIPT.json")).expect("tab");
    assert!(
        root_tab_true
            .runtime_dependencies
            .as_ref()
            .is_none_or(|deps| deps.is_empty()),
        "runtime_dependencies must exclude build/test even when poured"
    );
}
