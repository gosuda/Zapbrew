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
use zapbrew_ops::upgrade::{self, Args};
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
