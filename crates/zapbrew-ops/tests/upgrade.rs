mod support;

use support::fingerprint;

use std::collections::HashMap;
use std::io;
use std::str::FromStr;
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
use zapbrew_ops::transaction_test_support::{fail_install_after_unlink, fail_next_backup_cleanup};
use zapbrew_ops::upgrade::{self, Args, Mode};
use zapbrew_ops::{Ctx, Reporter};
use zapbrew_pour::LinkOptions;
use zapbrew_prefix::{
    CommandOutput, CommandRunner, CommandSpec, Env, EnvDetectInput, Keg, Prefix, Source,
    SourceVersions, Tab, pin,
};
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

fn env(temp: &TempDir, no_cleanup: bool) -> Env {
    let root = Utf8PathBuf::from_path_buf(temp.path().to_path_buf()).expect("utf8 temp");
    let mut vars = HashMap::from([
        (
            "HOMEBREW_PREFIX".to_owned(),
            root.join("prefix").to_string(),
        ),
        ("HOMEBREW_CACHE".to_owned(), root.join("cache").to_string()),
        ("HOMEBREW_TEMP".to_owned(), root.join("temp").to_string()),
    ]);
    if no_cleanup {
        vars.insert("HOMEBREW_NO_INSTALL_CLEANUP".to_owned(), "1".to_owned());
    }
    Env::detect_from(
        &EnvDetectInput {
            os: "linux".to_owned(),
            arch: "x86_64".to_owned(),
            home: root.join("home"),
            xdg_cache_home: None,
            vars,
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
    let payload = serde_json::to_vec(&formulae).expect("catalog payload");
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

fn context_with_casks(
    env: Env,
    formulae: Vec<Value>,
    cask_values: Vec<Value>,
) -> (Ctx, Arc<RecordingReporter>) {
    let formula_payload = serde_json::to_vec(&formulae).expect("catalog payload");
    let cask_payload = serde_json::to_vec(&cask_values).expect("cask payload");
    let catalog =
        Arc::new(Catalog::from_payload(&formula_payload, &env.bottle_tag).expect("catalog"));
    let casks = Arc::new(CaskCatalog::from_payload(&cask_payload, &env.bottle_tag).expect("casks"));
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

fn cask_archive(contents: &[u8]) -> Vec<u8> {
    let encoder = GzEncoder::new(Vec::new(), Compression::default());
    let mut archive = tar::Builder::new(encoder);
    let mut header = tar::Header::new_gnu();
    header.set_size(contents.len() as u64);
    header.set_mode(0o755);
    header.set_cksum();
    archive
        .append_data(&mut header, "tool", contents)
        .expect("cask tar entry");
    archive
        .into_inner()
        .expect("cask tar finish")
        .finish()
        .expect("cask gzip finish")
}

fn cask(token: &str, version: &str, url: &str, sha: &str) -> Value {
    json!({
        "token": token,
        "version": version,
        "sha256": sha,
        "url": url,
        "artifacts": [{"binary": ["tool"]}],
    })
}

fn installed_cask(env: &Env, token: &str, version: &str, pkg: bool) {
    let version_dir = env.caskroom.join(token).join(version);
    std::fs::create_dir_all(&version_dir).expect("cask version dir");
    std::fs::write(version_dir.join("tool"), b"old").expect("old cask tool");
    std::fs::create_dir_all(env.prefix.join("bin")).expect("prefix bin");
    let target = env.prefix.join("bin/tool");
    if !target.exists() {
        std::os::unix::fs::symlink(version_dir.join("tool"), &target).expect("old cask link");
    }
    let artifacts = if pkg {
        vec![json!({"kind": "pkg", "source": "Old.pkg"})]
    } else {
        vec![json!({"kind": "symlink", "target": target.as_str()})]
    };
    let record = json!({
        "schema": 1,
        "token": token,
        "version": version,
        "appdir": env.home.join("Applications").as_str(),
        "artifacts": artifacts,
        "uninstall": [],
        "zap": [],
    });
    let mut bytes = serde_json::to_vec_pretty(&record).expect("record");
    bytes.push(b'\n');
    std::fs::write(version_dir.join(".zapbrew-record.json"), bytes).expect("write record");
}

fn bottle(name: &str, version: &str) -> Vec<u8> {
    let encoder = GzEncoder::new(Vec::new(), Compression::default());
    let mut archive = tar::Builder::new(encoder);
    let body = format!("{name} {version}");
    let mut header = tar::Header::new_gnu();
    header.set_size(body.len() as u64);
    header.set_mode(0o755);
    header.set_cksum();
    archive
        .append_data(
            &mut header,
            format!("{name}/{version}/bin/{name}"),
            body.as_bytes(),
        )
        .expect("tar entry");
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

fn formula(name: &str, version: &str, revision: u32, scheme: u32, url: &str, sha: &str) -> Value {
    json!({
        "name": name,
        "full_name": name,
        "versions": {"stable": version, "bottle": true},
        "revision": revision,
        "version_scheme": scheme,
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

fn installed(env: &Env, name: &str, version: &str, scheme: u32, requested: bool) -> Keg {
    let keg = Keg::new(
        &env.cellar,
        FormulaName::from_str(name).expect("formula name"),
        PkgVersion::from_str(version).expect("pkg version"),
    )
    .expect("keg");
    std::fs::create_dir_all(keg.path().join("bin")).expect("keg dir");
    std::fs::write(keg.path().join(format!("bin/{name}")), b"old").expect("old binary");
    Tab {
        installed_on_request: requested,
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
    zapbrew_pour::link(&keg, &Prefix::new(env.clone()), LinkOptions::default()).expect("link old");
    keg
}

async fn mount(server: &MockServer, route: &str, bytes: &[u8], expected: u64) {
    Mock::given(method("GET"))
        .and(path(route))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(bytes.to_vec()))
        .expect(expected)
        .mount(server)
        .await;
}

fn named(name: &str) -> Args {
    Args {
        names: vec![name.to_owned()],
        ..Args::default()
    }
}

#[tokio::test]
async fn upgrades_real_bottle_preserves_request_state_and_removes_old_keg() {
    let server = MockServer::start().await;
    let tarball = bottle("foo", "1.1");
    mount(&server, "/foo.tar.gz", &tarball, 1).await;
    let temp = TempDir::new().expect("temp");
    let env = env(&temp, false);
    let old = installed(&env, "foo", "1.0", 0, false);
    let url = format!("{}/foo.tar.gz", server.uri());
    let (ctx, reporter) = context(
        env,
        vec![formula("foo", "1.1", 0, 0, &url, &digest(&tarball))],
    );

    upgrade::run(&ctx, named("foo")).await.expect("upgrade");

    let new = ctx.env.cellar.join("foo/1.1");
    assert!(new.exists());
    assert!(!old.path().exists());
    assert!(
        !Tab::load(new.join("INSTALL_RECEIPT.json"))
            .expect("new tab")
            .installed_on_request
    );
    assert_eq!(
        std::fs::canonicalize(ctx.env.linked.join("foo")).expect("linked"),
        std::fs::canonicalize(&new).expect("new keg")
    );
    let messages = reporter.take();
    assert_eq!(messages[0], "oh1:Upgrading 1 outdated package:");
    assert_eq!(messages[1], "print:foo  1.0 -> 1.1");
}

#[tokio::test]
async fn dry_run_scheme_bump_is_exact_and_performs_zero_download_or_mutation() {
    let server = MockServer::start().await;
    let tarball = bottle("schemedry", "1.0");
    mount(&server, "/scheme.tar.gz", &tarball, 0).await;
    let temp = TempDir::new().expect("temp");
    let env = env(&temp, false);
    let old = installed(&env, "schemedry", "9.0", 0, true);
    let url = format!("{}/scheme.tar.gz", server.uri());
    let (ctx, reporter) = context(
        env,
        vec![formula("schemedry", "1.0", 0, 1, &url, &digest(&tarball))],
    );

    upgrade::run(
        &ctx,
        Args {
            dry_run: true,
            ..named("schemedry")
        },
    )
    .await
    .expect("dry run");

    assert!(old.path().exists());
    assert!(!ctx.env.cellar.join("schemedry/1.0").exists());
    assert_eq!(
        reporter.take(),
        vec![
            "oh1:Would upgrade 1 outdated package:".to_owned(),
            "print:schemedry  9.0 -> 1.0".to_owned(),
        ]
    );
}

#[tokio::test]
async fn mixed_scheme_kegs_with_a_current_keg_are_not_upgraded() {
    let temp = TempDir::new().expect("temp");
    let env = env(&temp, false);
    let old = installed(&env, "mixedupgrade", "9.0", 0, true);
    zapbrew_pour::unlink(&old, &Prefix::new(env.clone())).expect("unlink old");
    let current = installed(&env, "mixedupgrade", "1.0", 1, true);
    let (ctx, reporter) = context(
        env,
        vec![formula(
            "mixedupgrade",
            "1.0",
            0,
            1,
            "http://unused",
            &"0".repeat(64),
        )],
    );

    upgrade::run(
        &ctx,
        Args {
            dry_run: true,
            ..named("mixedupgrade")
        },
    )
    .await
    .expect("up-to-date");

    assert!(old.path().exists());
    assert!(current.path().exists());
    assert_eq!(
        reporter.take(),
        vec![
            "opoo:mixedupgrade 1.0 is already installed and up-to-date.\nTo reinstall 1.0, run:\n  zapbrew reinstall mixedupgrade"
                .to_owned()
        ]
    );
}

#[tokio::test]
async fn named_pinned_is_refusal_and_all_mode_warns_and_skips() {
    let temp = TempDir::new().expect("temp");
    let env = env(&temp, false);
    let old = installed(&env, "pinnedfoo", "1.0", 0, true);
    pin(&env.pins, &old).expect("pin");
    let (ctx, reporter) = context(
        env,
        vec![formula(
            "pinnedfoo",
            "2.0",
            0,
            0,
            "http://unused",
            &"0".repeat(64),
        )],
    );

    let error = upgrade::run(&ctx, named("pinnedfoo"))
        .await
        .expect_err("named pinned");
    assert_eq!(
        error.to_string(),
        "Not upgrading 1 pinned package:\npinnedfoo 2.0"
    );

    upgrade::run(&ctx, Args::default())
        .await
        .expect("all pinned skipped");
    assert!(old.path().exists());
    assert_eq!(
        reporter.take(),
        vec![
            "opoo:Not upgrading 1 pinned package:".to_owned(),
            "print:pinnedfoo 2.0".to_owned(),
            "oh1:No packages to upgrade".to_owned(),
        ]
    );
}

#[tokio::test]
async fn no_install_cleanup_keeps_old_keg_after_new_commit() {
    let server = MockServer::start().await;
    let tarball = bottle("keepold", "2.0");
    mount(&server, "/keep.tar.gz", &tarball, 1).await;
    let temp = TempDir::new().expect("temp");
    let env = env(&temp, true);
    let old = installed(&env, "keepold", "1.0", 0, true);
    let url = format!("{}/keep.tar.gz", server.uri());
    let (ctx, _) = context(
        env,
        vec![formula("keepold", "2.0", 0, 0, &url, &digest(&tarball))],
    );

    upgrade::run(&ctx, named("keepold")).await.expect("upgrade");

    assert!(old.path().exists());
    assert!(ctx.env.cellar.join("keepold/2.0").exists());
}

#[tokio::test]
async fn transaction_failure_restores_normal_and_empty_old_link_surfaces() {
    let server = MockServer::start().await;
    let tarball = bottle("rollbackupgrade", "2.0");
    mount(&server, "/rollback.tar.gz", &tarball, 1).await;
    let temp = TempDir::new().expect("temp");
    let normal_env = env(&temp, false);
    let old = installed(&normal_env, "rollbackupgrade", "1.0", 0, true);
    let url = format!("{}/rollback.tar.gz", server.uri());
    let (ctx, _) = context(
        normal_env,
        vec![formula(
            "rollbackupgrade",
            "2.0",
            0,
            0,
            &url,
            &digest(&tarball),
        )],
    );
    fail_install_after_unlink("rollbackupgrade").expect("arm failure");

    let error = upgrade::run(&ctx, named("rollbackupgrade"))
        .await
        .expect_err("transaction failure");
    assert!(
        error
            .to_string()
            .contains("injected install failure after unlink")
    );
    assert!(old.path().exists());
    assert!(!ctx.env.cellar.join("rollbackupgrade/2.0").exists());
    assert_eq!(
        std::fs::canonicalize(ctx.env.linked.join("rollbackupgrade")).expect("linked"),
        std::fs::canonicalize(old.path()).expect("old keg")
    );

    {
        let server = MockServer::start().await;
        let tarball = bottle("emptyrollback", "2.0");
        mount(&server, "/empty-rollback.tar.gz", &tarball, 1).await;
        let temp = TempDir::new().expect("temp");
        let env = env(&temp, false);
        let old = installed(&env, "emptyrollback", "1.0", 0, true);
        zapbrew_pour::unlink(&old, &Prefix::new(env.clone())).expect("unlink normal surface");
        zapbrew_pour::link(
            &old,
            &Prefix::new(env.clone()),
            LinkOptions {
                keg_only: true,
                ..LinkOptions::default()
            },
        )
        .expect("empty link surface");
        let prefix_file = env.prefix.join("bin/emptyrollback");
        assert!(!prefix_file.exists());
        let url = format!("{}/empty-rollback.tar.gz", server.uri());
        let (ctx, _) = context(
            env,
            vec![formula(
                "emptyrollback",
                "2.0",
                0,
                0,
                &url,
                &digest(&tarball),
            )],
        );
        fail_install_after_unlink("emptyrollback").expect("arm failure");

        upgrade::run(&ctx, named("emptyrollback"))
            .await
            .expect_err("transaction failure");

        assert!(old.path().exists());
        assert!(ctx.env.linked.join("emptyrollback").exists());
        assert!(!ctx.env.cellar.join("emptyrollback/2.0").exists());
        assert!(!prefix_file.exists());
    }
}

#[tokio::test]
async fn cleanup_failure_warns_but_keeps_new_keg_active() {
    let server = MockServer::start().await;
    let tarball = bottle("cleanupupgrade", "2.0");
    mount(&server, "/cleanup.tar.gz", &tarball, 1).await;
    let temp = TempDir::new().expect("temp");
    let env = env(&temp, false);
    let old = installed(&env, "cleanupupgrade", "1.0", 0, true);
    let url = format!("{}/cleanup.tar.gz", server.uri());
    let (ctx, reporter) = context(
        env,
        vec![formula(
            "cleanupupgrade",
            "2.0",
            0,
            0,
            &url,
            &digest(&tarball),
        )],
    );
    fail_next_backup_cleanup("cleanupupgrade").expect("arm cleanup failure");

    upgrade::run(&ctx, named("cleanupupgrade"))
        .await
        .expect("committed upgrade");

    let new = ctx.env.cellar.join("cleanupupgrade/2.0");
    assert!(!old.path().exists());
    assert!(new.exists());
    assert_eq!(
        std::fs::canonicalize(ctx.env.linked.join("cleanupupgrade")).expect("linked"),
        std::fs::canonicalize(&new).expect("new keg")
    );
    assert!(reporter.take().iter().any(|message| {
        message.starts_with("opoo:Cleanup incomplete after upgrading cleanupupgrade:")
    }));
}

#[tokio::test]
async fn caveats_shown_by_default_and_dropped_when_quiet() {
    let server = MockServer::start().await;
    let tarball = bottle("foo", "1.1");
    mount(&server, "/foo.tar.gz", &tarball, 2).await;
    let url = format!("{}/foo.tar.gz", server.uri());
    let mut foo = formula("foo", "1.1", 0, 0, &url, &digest(&tarball));
    foo["caveats"] = json!("Config lives in $HOMEBREW_PREFIX/etc.");

    let default_temp = TempDir::new().expect("temp");
    let default_env = env(&default_temp, false);
    installed(&default_env, "foo", "1.0", 0, true);
    let (default_ctx, default) =
        context_flags(default_env, vec![foo.clone()], RecordingReporter::default());
    upgrade::run(&default_ctx, named("foo"))
        .await
        .expect("upgrade");
    let default_messages = default.take();
    assert!(default_messages.iter().any(|line| line == "ohai:Caveats"));

    let quiet_temp = TempDir::new().expect("temp");
    let quiet_env = env(&quiet_temp, false);
    installed(&quiet_env, "foo", "1.0", 0, true);
    let (quiet_ctx, quiet) =
        context_flags(quiet_env, vec![foo], RecordingReporter::with(true, false));
    upgrade::run(&quiet_ctx, named("foo"))
        .await
        .expect("upgrade");
    let quiet_messages = quiet.take();
    assert!(!quiet_messages.iter().any(|line| line == "ohai:Caveats"));
    assert!(
        !quiet_messages
            .iter()
            .any(|line| line.starts_with("print:Config lives in"))
    );
}

#[tokio::test]
async fn dry_run_does_not_mutate_prefix_with_ld_gcc_setup() {
    // On Linux, symlink_ld_so creates `<prefix>/lib/ld.so` and
    // setup_preferred_gcc_libs may write `etc/ld.so.conf.d/…`. Both must be
    // deferred past the dry-run return so the prefix is untouched.
    let server = MockServer::start().await;
    let tarball = bottle("schemedry", "1.0");
    mount(&server, "/scheme.tar.gz", &tarball, 0).await;
    let temp = TempDir::new().expect("temp");
    let env = env(&temp, false);
    installed(&env, "schemedry", "9.0", 0, true);
    let url = format!("{}/scheme.tar.gz", server.uri());
    let (ctx, _reporter) = context(
        env,
        vec![formula("schemedry", "1.0", 0, 1, &url, &digest(&tarball))],
    );

    let before = fingerprint(&ctx.env.prefix);
    upgrade::run(
        &ctx,
        Args {
            dry_run: true,
            ..named("schemedry")
        },
    )
    .await
    .expect("dry run");
    let after = fingerprint(&ctx.env.prefix);
    assert_eq!(before, after, "dry-run must not mutate the prefix");
}

// ---------------------------------------------------------------------------
// Per-tap lock contention tests
// ---------------------------------------------------------------------------

use zapbrew_ops::tap_lock_test_support;
use zapbrew_prefix::{LockGuard, PrefixError};

fn formula_with_tap(
    name: &str,
    version: &str,
    revision: u32,
    scheme: u32,
    url: &str,
    sha: &str,
    tap: &str,
) -> Value {
    let mut f = formula(name, version, revision, scheme, url, sha);
    f["tap"] = json!(tap);
    f
}

fn tap_lock(env: &Env, raw_tap: &str) -> LockGuard {
    let path = tap_lock_test_support::lock_path(&env.locks, raw_tap).expect("tap lock path");
    let dir = path.parent().expect("lock dir");
    let name = path.file_name().expect("lock file name");
    LockGuard::acquire(dir, name).expect("exclusive tap lock for test")
}

#[tokio::test]
async fn upgrade_blocked_by_exclusive_tap_lock() {
    let server = MockServer::start().await;
    let tarball = bottle("foo", "1.1");
    mount(&server, "/foo.tar.gz", &tarball, 0).await;
    let temp = TempDir::new().expect("temp");
    let env = env(&temp, false);
    let locks = env.locks.clone();
    installed(&env, "foo", "1.0", 0, false);
    let url = format!("{}/foo.tar.gz", server.uri());
    let (ctx, _) = context(
        env.clone(),
        vec![formula_with_tap(
            "foo",
            "1.1",
            0,
            0,
            &url,
            &digest(&tarball),
            "acme/tools",
        )],
    );

    let _lock = tap_lock(&env, "acme/tools");

    let error = upgrade::run(&ctx, named("foo"))
        .await
        .expect_err("upgrade must be blocked by exclusive tap lock");
    assert!(
        matches!(
            error,
            zapbrew_ops::OpError::Prefix(PrefixError::LockBusy { .. })
        ),
        "expected LockBusy from tap lock, got: {error}"
    );

    // Formula lock must not have been created — tap locks come first.
    assert!(
        !locks.join("foo.formula.lock").exists(),
        "formula lock must not be created when tap lock blocks"
    );
}

#[tokio::test]
async fn upgrade_dry_run_succeeds_under_exclusive_tap_lock_and_creates_no_locks() {
    let server = MockServer::start().await;
    let tarball = bottle("foo", "1.1");
    mount(&server, "/foo.tar.gz", &tarball, 0).await;
    let temp = TempDir::new().expect("temp");
    let env = env(&temp, false);
    let locks = env.locks.clone();
    installed(&env, "foo", "1.0", 0, true);
    let url = format!("{}/foo.tar.gz", server.uri());
    let (ctx, _) = context(
        env.clone(),
        vec![formula_with_tap(
            "foo",
            "1.1",
            0,
            0,
            &url,
            &digest(&tarball),
            "acme/tools",
        )],
    );

    // Hold an exclusive tap lock — dry-run must NOT be blocked.
    let _lock = tap_lock(&env, "acme/tools");

    upgrade::run(
        &ctx,
        Args {
            dry_run: true,
            ..named("foo")
        },
    )
    .await
    .expect("dry run must succeed under exclusive tap lock");

    assert!(
        !locks.join("foo.formula.lock").exists(),
        "dry-run must not create a formula lock"
    );
}

#[tokio::test]
async fn named_cask_upgrade_replaces_recorded_artifacts() {
    let server = MockServer::start().await;
    let archive = cask_archive(b"new");
    mount(&server, "/tool.tar.gz", &archive, 1).await;
    let temp = TempDir::new().expect("temp");
    let env = env(&temp, false);
    installed_cask(&env, "toolbox", "1.0", false);
    let url = format!("{}/tool.tar.gz", server.uri());
    let (ctx, _) = context_with_casks(
        env.clone(),
        vec![],
        vec![cask("toolbox", "2.0", &url, &digest(&archive))],
    );
    upgrade::run(
        &ctx,
        Args {
            names: vec!["toolbox".to_owned()],
            mode: Mode::Cask,
            ..Args::default()
        },
    )
    .await
    .expect("cask upgrade");
    assert!(!env.caskroom.join("toolbox/1.0").exists());
    assert!(env.caskroom.join("toolbox/2.0").is_dir());
    assert_eq!(
        std::fs::read(env.prefix.join("bin/tool")).expect("tool"),
        b"new"
    );
}

#[tokio::test]
async fn cask_upgrade_dry_run_has_no_request_or_mutation() {
    let server = MockServer::start().await;
    let archive = cask_archive(b"new");
    mount(&server, "/tool.tar.gz", &archive, 0).await;
    let temp = TempDir::new().expect("temp");
    let env = env(&temp, false);
    installed_cask(&env, "drybox", "1.0", false);
    let url = format!("{}/tool.tar.gz", server.uri());
    let (ctx, reporter) = context_with_casks(
        env.clone(),
        vec![],
        vec![cask("drybox", "2.0", &url, &digest(&archive))],
    );
    upgrade::run(
        &ctx,
        Args {
            names: vec!["drybox".to_owned()],
            mode: Mode::Cask,
            dry_run: true,
            ..Args::default()
        },
    )
    .await
    .expect("cask dry run");
    assert!(env.caskroom.join("drybox/1.0").is_dir());
    assert!(!env.caskroom.join("drybox/2.0").exists());
    assert!(
        !env.locks.join("drybox.cask.lock").exists(),
        "dry-run must not create a cask lock"
    );
    assert!(reporter.take().iter().any(|line| line.contains("drybox")));
}

#[tokio::test]
async fn unsafe_mixed_cask_refuses_before_formula_download() {
    let server = MockServer::start().await;
    let formula_archive = bottle("foo", "2.0");
    mount(&server, "/foo.tar.gz", &formula_archive, 0).await;
    let temp = TempDir::new().expect("temp");
    let env = env(&temp, false);
    installed(&env, "foo", "1.0", 0, true);
    installed_cask(&env, "unsafe-box", "1.0", true);
    let formula_url = format!("{}/foo.tar.gz", server.uri());
    let (ctx, _) = context_with_casks(
        env.clone(),
        vec![formula(
            "foo",
            "2.0",
            0,
            0,
            &formula_url,
            &digest(&formula_archive),
        )],
        vec![cask(
            "unsafe-box",
            "2.0",
            "http://unused.invalid/cask.tar.gz",
            "no_check",
        )],
    );
    let error = upgrade::run(
        &ctx,
        Args {
            names: vec!["foo".to_owned(), "unsafe-box".to_owned()],
            ..Args::default()
        },
    )
    .await
    .expect_err("unsafe cask");
    assert!(error.to_string().contains("irreversible pkg"));
    assert!(env.cellar.join("foo/1.0").is_dir());
    assert!(!env.cellar.join("foo/2.0").exists());
}

#[tokio::test]
async fn mixed_cask_download_failure_precedes_formula_mutation() {
    let server = MockServer::start().await;
    let formula_archive = bottle("foo", "2.0");
    mount(&server, "/foo.tar.gz", &formula_archive, 1).await;
    Mock::given(method("GET"))
        .and(path("/cask.tar.gz"))
        .respond_with(ResponseTemplate::new(500))
        .expect(1..)
        .mount(&server)
        .await;
    let temp = TempDir::new().expect("temp");
    let environment = env(&temp, false);
    installed(&environment, "foo", "1.0", 0, true);
    installed_cask(&environment, "safe-box", "1.0", false);
    let (ctx, _) = context_with_casks(
        environment.clone(),
        vec![formula(
            "foo",
            "2.0",
            0,
            0,
            &format!("{}/foo.tar.gz", server.uri()),
            &digest(&formula_archive),
        )],
        vec![cask(
            "safe-box",
            "2.0",
            &format!("{}/cask.tar.gz", server.uri()),
            "no_check",
        )],
    );

    upgrade::run(
        &ctx,
        Args {
            names: vec!["foo".to_owned(), "safe-box".to_owned()],
            ..Args::default()
        },
    )
    .await
    .expect_err("cask download failure");

    assert!(environment.cellar.join("foo/1.0").is_dir());
    assert!(!environment.cellar.join("foo/2.0").exists());
    assert!(environment.caskroom.join("safe-box/1.0").is_dir());
    assert!(!environment.caskroom.join("safe-box/2.0").exists());
}
#[tokio::test]
async fn unsafe_cask_refuses_before_deferred_migration_in_any_order() {
    let server = MockServer::start().await;
    mount(&server, "/formula_tap_migrations.jws.json", b"unused", 0).await;
    let temp = TempDir::new().expect("temp");
    let mut environment = env(&temp, false);
    environment.api_domain = server.uri();
    installed_cask(&environment, "unsafe-box", "1.0", true);
    let (ctx, _) = context_with_casks(
        environment.clone(),
        vec![],
        vec![cask(
            "unsafe-box",
            "2.0",
            "http://unused.invalid/cask.tar.gz",
            "no_check",
        )],
    );

    for names in [
        vec!["missing".to_owned(), "unsafe-box".to_owned()],
        vec!["unsafe-box".to_owned(), "missing".to_owned()],
    ] {
        let error = upgrade::run(
            &ctx,
            Args {
                names,
                ..Args::default()
            },
        )
        .await
        .expect_err("unsafe cask");
        assert!(error.to_string().contains("irreversible pkg"));
        assert!(!environment.locks.join("missing.formula.lock").exists());
        assert!(!environment.locks.join("unsafe-box.cask.lock").exists());
        assert!(environment.caskroom.join("unsafe-box/1.0").is_dir());
        assert!(!environment.caskroom.join("unsafe-box/2.0").exists());
    }
}

#[tokio::test]
async fn up_to_date_unsafe_cask_does_not_preempt_deferred_migration() {
    let server = MockServer::start().await;
    mount(
        &server,
        "/formula_tap_migrations.jws.json",
        b"invalid signed payload",
        // The invalid primary response triggers the documented default-mirror retry.
        2,
    )
    .await;
    let temp = TempDir::new().expect("temp");
    let mut environment = env(&temp, false);
    environment.api_domain = server.uri();
    installed_cask(&environment, "current-box", "2.0", true);
    let (ctx, _) = context_with_casks(
        environment.clone(),
        vec![],
        vec![cask(
            "current-box",
            "2.0",
            "http://unused.invalid/cask.tar.gz",
            "no_check",
        )],
    );

    let error = upgrade::run(
        &ctx,
        Args {
            names: vec!["missing".to_owned(), "current-box".to_owned()],
            ..Args::default()
        },
    )
    .await
    .expect_err("invalid migration response");

    assert!(!error.to_string().contains("irreversible pkg"));
    assert!(environment.caskroom.join("current-box/2.0").is_dir());
    assert!(!environment.locks.join("current-box.cask.lock").exists());
}
#[tokio::test]
async fn named_latest_cask_is_greedy_but_bare_upgrade_skips_it() {
    let temp = TempDir::new().expect("temp");
    let env = env(&temp, false);
    installed_cask(&env, "rolling", "1.0", false);
    let value = cask(
        "rolling",
        "latest",
        "http://unused.invalid/latest.tar.gz",
        "no_check",
    );
    let (bare_ctx, bare_reporter) = context_with_casks(env.clone(), vec![], vec![value.clone()]);
    upgrade::run(
        &bare_ctx,
        Args {
            mode: Mode::Cask,
            dry_run: true,
            ..Args::default()
        },
    )
    .await
    .expect("bare dry run");
    assert!(
        !bare_reporter
            .take()
            .iter()
            .any(|line| line.contains("rolling"))
    );
    let (named_ctx, named_reporter) = context_with_casks(env, vec![], vec![value]);
    upgrade::run(
        &named_ctx,
        Args {
            names: vec!["rolling".to_owned()],
            mode: Mode::Cask,
            dry_run: true,
            ..Args::default()
        },
    )
    .await
    .expect("named dry run");
    assert!(
        named_reporter
            .take()
            .iter()
            .any(|line| line.contains("rolling"))
    );
}

#[tokio::test]
async fn cask_dry_run_refuses_unapproved_appdir() {
    let temp = TempDir::new().expect("temp");
    let env = env(&temp, false);
    installed_cask(&env, "dry-appdir", "1.0", false);
    let (ctx, _) = context_with_casks(
        env,
        vec![],
        vec![cask(
            "dry-appdir",
            "2.0",
            "http://unused.invalid/cask.tar.gz",
            "no_check",
        )],
    );
    let error = upgrade::run(
        &ctx,
        Args {
            names: vec!["dry-appdir".to_owned()],
            mode: Mode::Cask,
            appdir: Some("/etc".into()),
            dry_run: true,
            ..Args::default()
        },
    )
    .await
    .expect_err("unapproved appdir");
    assert!(error.to_string().contains("outside approved roots"));
}
