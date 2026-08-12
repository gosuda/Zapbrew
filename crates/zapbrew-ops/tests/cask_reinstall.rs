mod support;

use std::fs;
use std::sync::Arc;

use camino::Utf8PathBuf;
use flate2::Compression;
use flate2::write::GzEncoder;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use support::{Fixture, PanicRunner};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};
use zapbrew_ops::cask::{install, reinstall};
use zapbrew_ops::{Ctx, OpError};

fn tar_gz(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut builder = tar::Builder::new(GzEncoder::new(Vec::new(), Compression::default()));
    for (name, contents) in entries {
        let mut header = tar::Header::new_gnu();
        header.set_size(contents.len() as u64);
        header.set_mode(0o644);
        header.set_cksum();
        builder
            .append_data(&mut header, name, *contents)
            .expect("tar entry");
    }
    let encoder = builder.into_inner().expect("finish tar");
    encoder.finish().expect("finish gzip")
}

fn sha_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

async fn serve(server: &MockServer, route: &str, body: Vec<u8>) -> String {
    Mock::given(method("GET"))
        .and(path(route))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(body))
        .mount(server)
        .await;
    format!("{}{route}", server.uri())
}

fn cask(token: &str, url: &str, sha: &str, artifacts: Vec<Value>) -> Value {
    json!({
        "token": token,
        "version": "1.0",
        "sha256": sha,
        "url": url,
        "artifacts": artifacts,
    })
}

fn cask_v(token: &str, version: &str, url: &str, sha: &str, artifacts: Vec<Value>) -> Value {
    let mut value = cask(token, url, sha, artifacts);
    value["version"] = json!(version);
    value
}

fn err(result: Result<(), OpError>) -> OpError {
    match result {
        Ok(()) => panic!("expected Err, got Ok"),
        Err(error) => error,
    }
}

fn ctx(fixture: &Fixture, casks: Vec<Value>) -> Ctx {
    let (ctx, _reporter) =
        fixture.context_casks(casks, Arc::new(PanicRunner), reqwest::Client::new());
    ctx
}

fn install_args(tokens: &[&str], appdir: &Utf8PathBuf, force: bool) -> install::Args {
    install::Args {
        tokens: tokens.iter().map(|t| (*t).to_owned()).collect(),
        appdir: Some(appdir.clone()),
        force,
    }
}

fn reinstall_args(tokens: &[&str], appdir: Option<&str>) -> reinstall::Args {
    reinstall::Args {
        tokens: tokens.iter().map(|t| (*t).to_owned()).collect(),
        appdir: appdir.map(Utf8PathBuf::from),
    }
}

fn record_appdir(prefix: &Utf8PathBuf, name: &str) -> Value {
    json!({
        "kind": "symlink",
        "target": format!("{}/bin/{name}", prefix),
    })
}

/// Seed a typed install record into a promoted version tree.
fn seed_record(fixture: &Fixture, token: &str, version: &str, appdir: &str, artifacts: &[Value]) {
    let version_dir = fixture.env.caskroom.join(token).join(version);
    fs::create_dir_all(&version_dir).expect("version dir");
    let record = json!({
        "schema": 1,
        "token": token,
        "version": version,
        "appdir": appdir,
        "artifacts": artifacts,
        "uninstall": [],
        "zap": [],
    });
    let mut bytes = serde_json::to_vec_pretty(&record).expect("record json");
    bytes.push(b'\n');
    fs::write(version_dir.join(".zapbrew-record.json"), bytes).expect("seed record");
}

fn read_record_appdir(fixture: &Fixture, token: &str, version: &str) -> String {
    let path = fixture
        .env
        .caskroom
        .join(token)
        .join(version)
        .join(".zapbrew-record.json");
    let bytes = fs::read(path).expect("read record");
    let value: Value = serde_json::from_slice(&bytes).expect("parse record");
    value["appdir"].as_str().expect("appdir string").to_owned()
}

fn appdir(fixture: &Fixture, rel: &str) -> Utf8PathBuf {
    fixture.env.home.join(rel)
}

#[tokio::test]
async fn reinstall_refuses_missing_cask() {
    let fixture = Fixture::new();
    let context = ctx(&fixture, vec![]);
    let error = err(reinstall::run(&context, reinstall_args(&["ghost"], None)).await);
    assert_eq!(error.to_string(), "Cask 'ghost' is unavailable.");
}

#[tokio::test]
async fn reinstall_refuses_qualified_cask() {
    let fixture = Fixture::new();
    let context = ctx(&fixture, vec![]);
    let error =
        err(reinstall::run(&context, reinstall_args(&["homebrew/cask/firefox"], None)).await);
    assert_eq!(
        error.to_string(),
        "Cask 'homebrew/cask/firefox' is unavailable."
    );
}

#[tokio::test]
async fn reinstall_refuses_not_installed() {
    let fixture = Fixture::new();
    let value = cask(
        "gone",
        "http://unused",
        "no_check",
        vec![json!({"binary": ["tool"]})],
    );
    let context = ctx(&fixture, vec![value]);
    let error = err(reinstall::run(&context, reinstall_args(&["gone"], None)).await);
    assert_eq!(error.to_string(), "Cask 'gone' is not installed.");
}

#[tokio::test]
async fn reinstall_refuses_missing_record() {
    let fixture = Fixture::new();
    let value = cask(
        "norecord",
        "http://unused",
        "no_check",
        vec![json!({"binary": ["tool"]})],
    );
    fs::create_dir_all(fixture.env.caskroom.join("norecord").join("1.0")).expect("version dir");
    let context = ctx(&fixture, vec![value]);
    let error = err(reinstall::run(&context, reinstall_args(&["norecord"], None)).await);
    assert!(
        error.to_string().contains(".zapbrew-record.json"),
        "expected record-missing error, got: {error}"
    );
}

#[tokio::test]
async fn reinstall_refuses_malformed_record() {
    let fixture = Fixture::new();
    let value = cask(
        "badrecord",
        "http://unused",
        "no_check",
        vec![json!({"binary": ["tool"]})],
    );
    let version_dir = fixture.env.caskroom.join("badrecord").join("1.0");
    fs::create_dir_all(&version_dir).expect("version dir");
    fs::write(version_dir.join(".zapbrew-record.json"), b"not json").expect("bad record");
    let context = ctx(&fixture, vec![value]);
    let error = err(reinstall::run(&context, reinstall_args(&["badrecord"], None)).await);
    assert!(
        error.to_string().contains("cask install record"),
        "expected malformed record error, got: {error}"
    );
}

#[tokio::test]
async fn reinstall_refuses_predecessor_pkg() {
    let fixture = Fixture::new();
    let value = cask(
        "pkgcask",
        "http://unused",
        "no_check",
        vec![json!({"binary": ["tool"]})],
    );
    seed_record(
        &fixture,
        "pkgcask",
        "1.0",
        "/Applications",
        &[json!({"kind": "pkg", "source": "pkgcask.pkg"})],
    );
    let override_appdir = appdir(&fixture, "Other/Applications");
    let context = ctx(&fixture, vec![value]);
    let error = err(reinstall::run(
        &context,
        reinstall_args(&["pkgcask"], Some(override_appdir.as_str())),
    )
    .await);
    assert_eq!(
        error.to_string(),
        "Cask 'pkgcask' has an irreversible pkg install and cannot be reinstalled."
    );
}

#[tokio::test]
async fn reinstall_refuses_predecessor_uninstall_directives() {
    let fixture = Fixture::new();
    let value = cask(
        "uninstallcask",
        "http://unused",
        "no_check",
        vec![json!({"binary": ["tool"]})],
    );
    let version_dir = fixture.env.caskroom.join("uninstallcask").join("1.0");
    fs::create_dir_all(&version_dir).expect("version dir");
    let record = json!({
        "schema": 1,
        "token": "uninstallcask",
        "version": "1.0",
        "appdir": "/Applications",
        "artifacts": [record_appdir(&fixture.env.prefix, "tool")],
        "uninstall": [[{"delete": ["/tmp/example"]}]],
        "zap": [],
    });
    let mut bytes = serde_json::to_vec_pretty(&record).expect("record json");
    bytes.push(b'\n');
    fs::write(version_dir.join(".zapbrew-record.json"), bytes).expect("seed record");
    let context = ctx(&fixture, vec![value]);
    let error = err(reinstall::run(&context, reinstall_args(&["uninstallcask"], None)).await);
    assert_eq!(
        error.to_string(),
        "Cask 'uninstallcask' has nonempty uninstall directives and cannot be reinstalled."
    );
}

#[tokio::test]
async fn reinstall_requires_override_for_divergent_appdirs() {
    let server = MockServer::start().await;
    let body = tar_gz(&[("tool", b"replacement")]);
    let sha = sha_hex(&body);
    let url = serve(&server, "/splitapp.tar.gz", body).await;
    let fixture = Fixture::new();
    let value = cask("splitapp", &url, &sha, vec![json!({"binary": ["tool"]})]);
    let appdir_a = appdir(&fixture, "A/Applications");
    let appdir_b = appdir(&fixture, "B/Applications");
    seed_record(
        &fixture,
        "splitapp",
        "1.0",
        appdir_a.as_str(),
        &[record_appdir(&fixture.env.prefix, "tool")],
    );
    seed_record(
        &fixture,
        "splitapp",
        "2.0",
        appdir_b.as_str(),
        &[record_appdir(&fixture.env.prefix, "tool")],
    );
    let context = ctx(&fixture, vec![value]);
    let error = err(reinstall::run(&context, reinstall_args(&["splitapp"], None)).await);
    assert!(
        error.to_string().contains("different appdirs"),
        "expected divergent appdirs error, got: {error}"
    );
    assert!(
        error.to_string().contains(appdir_a.as_str()),
        "expected appdir A in message, got: {error}"
    );
    assert!(
        error.to_string().contains(appdir_b.as_str()),
        "expected appdir B in message, got: {error}"
    );
    let appdir_override = appdir(&fixture, "Chosen/Applications");
    reinstall::run(
        &context,
        reinstall_args(&["splitapp"], Some(appdir_override.as_str())),
    )
    .await
    .expect("explicit appdir resolves divergent records");
    assert_eq!(
        read_record_appdir(&fixture, "splitapp", "1.0"),
        appdir_override.as_str()
    );
}

#[tokio::test]
async fn reinstall_uses_recorded_appdir_when_omitted() {
    let server = MockServer::start().await;
    let body = tar_gz(&[("tool", b"one")]);
    let sha = sha_hex(&body);
    let url = serve(&server, "/tool.tar.gz", body.clone()).await;

    let fixture = Fixture::new();
    let appdir = appdir(&fixture, "Applications");
    let value = cask("keeper", &url, &sha, vec![json!({"binary": ["tool"]})]);

    let install_ctx = ctx(&fixture, vec![value.clone()]);
    install::run(&install_ctx, install_args(&["keeper"], &appdir, false))
        .await
        .expect("install");
    assert_eq!(
        read_record_appdir(&fixture, "keeper", "1.0"),
        appdir.as_str()
    );

    let reinstall_ctx = ctx(&fixture, vec![value]);
    reinstall::run(&reinstall_ctx, reinstall_args(&["keeper"], None))
        .await
        .expect("reinstall");
    assert_eq!(
        read_record_appdir(&fixture, "keeper", "1.0"),
        appdir.as_str()
    );
}

#[tokio::test]
async fn reinstall_explicit_appdir_overrides_recorded() {
    let server = MockServer::start().await;
    let body = tar_gz(&[("tool", b"one")]);
    let sha = sha_hex(&body);
    let url = serve(&server, "/tool.tar.gz", body.clone()).await;

    let fixture = Fixture::new();
    let old_appdir = appdir(&fixture, "Applications");
    let new_appdir = appdir(&fixture, "Other/Applications");
    let value = cask("mover", &url, &sha, vec![json!({"binary": ["tool"]})]);

    let install_ctx = ctx(&fixture, vec![value.clone()]);
    install::run(&install_ctx, install_args(&["mover"], &old_appdir, false))
        .await
        .expect("install");

    let reinstall_ctx = ctx(&fixture, vec![value]);
    reinstall::run(
        &reinstall_ctx,
        reinstall_args(&["mover"], Some(new_appdir.as_str())),
    )
    .await
    .expect("reinstall with changed appdir");
    assert_eq!(
        read_record_appdir(&fixture, "mover", "1.0"),
        new_appdir.as_str()
    );
}

#[tokio::test]
async fn reinstall_two_tokens_preserve_different_appdirs() {
    let server = MockServer::start().await;
    let body_a = tar_gz(&[("toola", b"a")]);
    let body_b = tar_gz(&[("toolb", b"b")]);
    let sha_a = sha_hex(&body_a);
    let sha_b = sha_hex(&body_b);
    let url_a = serve(&server, "/a.tar.gz", body_a).await;
    let url_b = serve(&server, "/b.tar.gz", body_b).await;

    let fixture = Fixture::new();
    let appdir_a = appdir(&fixture, "A/Applications");
    let appdir_b = appdir(&fixture, "B/Applications");
    let value_a = cask("tokena", &url_a, &sha_a, vec![json!({"binary": ["toola"]})]);
    let value_b = cask("tokenb", &url_b, &sha_b, vec![json!({"binary": ["toolb"]})]);

    let install_ctx = ctx(&fixture, vec![value_a.clone(), value_b.clone()]);
    install::run(&install_ctx, install_args(&["tokena"], &appdir_a, false))
        .await
        .expect("install a");
    install::run(&install_ctx, install_args(&["tokenb"], &appdir_b, false))
        .await
        .expect("install b");

    let reinstall_ctx = ctx(&fixture, vec![value_a, value_b]);
    reinstall::run(&reinstall_ctx, reinstall_args(&["tokena", "tokenb"], None))
        .await
        .expect("reinstall both");
    assert_eq!(
        read_record_appdir(&fixture, "tokena", "1.0"),
        appdir_a.as_str()
    );
    assert_eq!(
        read_record_appdir(&fixture, "tokenb", "1.0"),
        appdir_b.as_str()
    );
}

#[tokio::test]
async fn reinstall_same_version_success() {
    let server = MockServer::start().await;
    let body = tar_gz(&[("tool", b"one")]);
    let sha = sha_hex(&body);
    let url = serve(&server, "/tool.tar.gz", body.clone()).await;

    let fixture = Fixture::new();
    let appdir = appdir(&fixture, "Applications");
    let value = cask("samey", &url, &sha, vec![json!({"binary": ["tool"]})]);

    let install_ctx = ctx(&fixture, vec![value.clone()]);
    install::run(&install_ctx, install_args(&["samey"], &appdir, false))
        .await
        .expect("install");

    // Reinstall with the same cask catalog; the archive is mounted again.
    let url2 = serve(&server, "/tool2.tar.gz", body).await;
    let value2 = cask("samey", &url2, &sha, vec![json!({"binary": ["tool"]})]);
    let reinstall_ctx = ctx(&fixture, vec![value2]);
    reinstall::run(&reinstall_ctx, reinstall_args(&["samey"], None))
        .await
        .expect("reinstall same version");

    let version_dir = fixture.env.caskroom.join("samey").join("1.0");
    assert!(version_dir.exists());
    assert!(fixture.env.prefix.join("bin/tool").is_symlink());
}

#[tokio::test]
async fn reinstall_cross_version_success() {
    let server = MockServer::start().await;
    let v1 = tar_gz(&[("tool", b"one")]);
    let v2 = tar_gz(&[("tool", b"two")]);
    let v1_sha = sha_hex(&v1);
    let v2_sha = sha_hex(&v2);
    let v1_url = serve(&server, "/v1.tar.gz", v1).await;
    let v2_url = serve(&server, "/v2.tar.gz", v2).await;

    let fixture = Fixture::new();
    let appdir = appdir(&fixture, "Applications");
    let value_v1 = cask(
        "crossy",
        &v1_url,
        &v1_sha,
        vec![json!({"binary": ["tool"]})],
    );
    let value_v2 = cask_v(
        "crossy",
        "2.0",
        &v2_url,
        &v2_sha,
        vec![json!({"binary": ["tool"]})],
    );

    let install_ctx = ctx(&fixture, vec![value_v1]);
    install::run(&install_ctx, install_args(&["crossy"], &appdir, false))
        .await
        .expect("install v1");
    assert!(fixture.env.caskroom.join("crossy").join("1.0").exists());

    let reinstall_ctx = ctx(&fixture, vec![value_v2]);
    reinstall::run(&reinstall_ctx, reinstall_args(&["crossy"], None))
        .await
        .expect("reinstall to v2");
    assert!(!fixture.env.caskroom.join("crossy").join("1.0").exists());
    assert!(fixture.env.caskroom.join("crossy").join("2.0").exists());
    assert!(fixture.env.prefix.join("bin/tool").is_symlink());
}

#[tokio::test]
async fn reinstall_failure_rollback_restores_old_version() {
    let server = MockServer::start().await;
    let v1 = tar_gz(&[("tool", b"old")]);
    let v1_sha = sha_hex(&v1);
    let v1_url = serve(&server, "/v1.tar.gz", v1).await;

    // v2 declares two binaries but the archive only contains one, so apply fails
    // partway through and the journaled rollback must restore the v1 target.
    let v2 = tar_gz(&[("tool", b"new")]);
    let v2_sha = sha_hex(&v2);
    let v2_url = serve(&server, "/v2.tar.gz", v2).await;

    let fixture = Fixture::new();
    let appdir = appdir(&fixture, "Applications");
    let value_v1 = cask("rolly", &v1_url, &v1_sha, vec![json!({"binary": ["tool"]})]);
    let value_v2 = cask_v(
        "rolly",
        "2.0",
        &v2_url,
        &v2_sha,
        vec![json!({"binary": ["tool"]}), json!({"binary": ["missing"]})],
    );

    let install_ctx = ctx(&fixture, vec![value_v1]);
    install::run(&install_ctx, install_args(&["rolly"], &appdir, false))
        .await
        .expect("install v1");
    assert_eq!(
        fs::read(fixture.env.caskroom.join("rolly").join("1.0").join("tool")).expect("v1 tool"),
        b"old"
    );
    let deployed = fixture.env.prefix.join("bin/tool");
    assert!(
        fs::symlink_metadata(&deployed)
            .expect("initial deployed target metadata")
            .file_type()
            .is_symlink(),
        "v1 install must deploy a symlink"
    );
    assert_eq!(
        fs::read_link(&deployed).expect("initial deployed target"),
        fixture.env.caskroom.join("rolly").join("1.0").join("tool")
    );

    let reinstall_ctx = ctx(&fixture, vec![value_v2]);
    let error = err(reinstall::run(&reinstall_ctx, reinstall_args(&["rolly"], None)).await);
    assert!(
        error.to_string().contains("missing"),
        "expected missing source error, got: {error}"
    );
    assert!(
        !matches!(&error, OpError::RollbackIncomplete { .. }),
        "rollback must return the original error: {error}"
    );

    assert!(
        fixture.env.caskroom.join("rolly").join("1.0").exists(),
        "old version must survive rollback"
    );
    assert!(
        !fixture.env.caskroom.join("rolly").join("2.0").exists(),
        "new version must not be promoted"
    );
    assert!(
        fixture
            .env
            .caskroom
            .join("rolly")
            .join("1.0")
            .join(".zapbrew-record.json")
            .is_file(),
        "predecessor record must survive rollback"
    );
    assert!(
        fs::symlink_metadata(&deployed)
            .expect("deployed target metadata")
            .file_type()
            .is_symlink(),
        "deployed symlink must be restored"
    );
    assert_eq!(
        fs::read_link(&deployed).expect("deployed target"),
        fixture.env.caskroom.join("rolly").join("1.0").join("tool")
    );
    assert_eq!(
        fs::read(&deployed).expect("restored deployed target"),
        b"old"
    );
}
