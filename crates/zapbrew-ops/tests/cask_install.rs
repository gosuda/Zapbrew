mod support;

use camino::{Utf8Path, Utf8PathBuf};
use flate2::Compression;
use flate2::write::GzEncoder;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::fs;
use std::io::Write;
use std::sync::{Arc, Mutex};
use support::{Fixture, fingerprint};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};
use zapbrew_ops::cask::install::{self, Args};
use zapbrew_ops::cask::uninstall;
use zapbrew_ops::{Ctx, OpError};
use zapbrew_prefix::{CommandOutput, CommandRunner, CommandSpec};

struct PanicRunner;
impl CommandRunner for PanicRunner {
    fn run(&self, spec: &CommandSpec) -> Result<CommandOutput, std::io::Error> {
        panic!("host command must not run: {:?}", spec.program())
    }
}

#[derive(Default)]
struct ScriptedRunner {
    calls: Mutex<Vec<Vec<String>>>,
    responses: Mutex<Vec<Result<Vec<u8>, ()>>>,
}

impl ScriptedRunner {
    fn with(responses: Vec<Result<Vec<u8>, ()>>) -> Arc<Self> {
        Arc::new(Self {
            calls: Mutex::new(Vec::new()),
            responses: Mutex::new(responses),
        })
    }

    fn argv(&self) -> Vec<Vec<String>> {
        self.calls.lock().expect("calls").clone()
    }
}

impl CommandRunner for ScriptedRunner {
    fn run(&self, spec: &CommandSpec) -> Result<CommandOutput, std::io::Error> {
        let mut call = vec![spec.program().to_string_lossy().into_owned()];
        call.extend(
            spec.arguments()
                .iter()
                .map(|a| a.to_string_lossy().into_owned()),
        );
        self.calls.lock().expect("calls").push(call);
        let mut responses = self.responses.lock().expect("responses");
        let next = if responses.is_empty() {
            Ok(Vec::new())
        } else {
            responses.remove(0)
        };
        match next {
            Ok(stdout) => Ok(CommandOutput::new(status(true), stdout, Vec::new())),
            Err(()) => Ok(CommandOutput::new(
                status(false),
                Vec::new(),
                b"boom".to_vec(),
            )),
        }
    }
}

fn status(success: bool) -> std::process::ExitStatus {
    use std::os::unix::process::ExitStatusExt;
    std::process::ExitStatus::from_raw(if success { 0 } else { 256 })
}

fn sha_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Build a `.tar.gz` whose top-level entries are the given `(relative, contents)` files.
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

/// Build a `.tar.gz` with one symlink entry, to prove link entries refuse.
fn tar_gz_with_symlink(file: (&str, &[u8]), link: (&str, &str)) -> Vec<u8> {
    let mut builder = tar::Builder::new(GzEncoder::new(Vec::new(), Compression::default()));
    let mut header = tar::Header::new_gnu();
    header.set_size(file.1.len() as u64);
    header.set_mode(0o644);
    header.set_cksum();
    builder
        .append_data(&mut header, file.0, file.1)
        .expect("tar file entry");
    let mut link_header = tar::Header::new_gnu();
    link_header.set_entry_type(tar::EntryType::Symlink);
    link_header.set_size(0);
    link_header.set_mode(0o777);
    link_header.set_cksum();
    builder
        .append_link(&mut link_header, link.0, link.1)
        .expect("tar link entry");
    let encoder = builder.into_inner().expect("finish tar");
    encoder.finish().expect("finish gzip")
}

fn zip_bytes(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut cursor = std::io::Cursor::new(Vec::new());
    {
        let mut writer = zip::ZipWriter::new(&mut cursor);
        let options: zip::write::FileOptions<'_, ()> =
            zip::write::FileOptions::default().unix_permissions(0o644);
        for (name, contents) in entries {
            writer.start_file(*name, options).expect("zip entry");
            writer.write_all(contents).expect("zip write");
        }
        writer.finish().expect("zip finish");
    }
    cursor.into_inner()
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

fn install_args(tokens: &[&str], appdir: &Utf8Path, force: bool) -> Args {
    Args {
        tokens: tokens.iter().map(|t| (*t).to_owned()).collect(),
        appdir: Some(appdir.to_owned()),
        force,
    }
}

fn err(result: Result<(), OpError>) -> OpError {
    match result {
        Ok(()) => panic!("expected Err, got Ok"),
        Err(error) => error,
    }
}

/// Seed a typed install record into a promoted version tree, mirroring what a
/// real install writes before the atomic promote. `artifacts` are the persisted
/// `DeployedArtifact` JSON entries in application order.
fn seed_record(fixture: &Fixture, token: &str, version: &str, appdir: &str, artifacts: &[Value]) {
    let version_dir = fixture.env.caskroom.join(token).join(version);
    std::fs::create_dir_all(&version_dir).expect("version dir");
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
    std::fs::write(version_dir.join(".zapbrew-record.json"), bytes).expect("seed record");
}

fn seed_record_with_uninstall(
    fixture: &Fixture,
    token: &str,
    version: &str,
    appdir: &str,
    artifacts: &[Value],
) {
    let version_dir = fixture.env.caskroom.join(token).join(version);
    std::fs::create_dir_all(&version_dir).expect("version dir");
    let record = json!({
        "schema": 1,
        "token": token,
        "version": version,
        "appdir": appdir,
        "artifacts": artifacts,
        "uninstall": [[{"delete": ["/tmp/example"]}]],
        "zap": [],
    });
    let mut bytes = serde_json::to_vec_pretty(&record).expect("record json");
    bytes.push(b'\n');
    std::fs::write(version_dir.join(".zapbrew-record.json"), bytes).expect("seed record");
}

#[tokio::test]
async fn force_replacement_refuses_irreversible_effects_before_download() {
    for case in ["new-pkg", "old-pkg", "old-uninstall"] {
        let server = MockServer::start().await;
        let fixture = Fixture::new().macos();
        let appdir = fixture.env.home.join("Applications");
        let token = format!("force-{case}");
        let artifacts = if case == "new-pkg" {
            vec![json!({"pkg": ["App.pkg"]})]
        } else {
            vec![json!({"binary": ["tool"]})]
        };
        let value = cask(
            &token,
            &format!("{}/unused.tar.gz", server.uri()),
            "no_check",
            artifacts,
        );
        let old_artifacts = if case == "old-pkg" {
            vec![json!({"kind": "pkg", "source": "Old.pkg"})]
        } else {
            vec![json!({
                "kind": "symlink",
                "target": fixture.env.prefix.join("bin/tool").as_str()
            })]
        };
        if case == "old-uninstall" {
            seed_record_with_uninstall(&fixture, &token, "0.9", appdir.as_str(), &old_artifacts);
        } else {
            seed_record(&fixture, &token, "0.9", appdir.as_str(), &old_artifacts);
        }
        let old_record = fixture
            .env
            .caskroom
            .join(&token)
            .join("0.9")
            .join(".zapbrew-record.json");
        let (ctx, _reporter) =
            fixture.context_casks(vec![value], Arc::new(PanicRunner), reqwest::Client::new());
        let error = err(install::run(&ctx, install_args(&[&token], &appdir, true)).await);
        assert!(
            error.to_string().contains("pkg") || error.to_string().contains("uninstall"),
            "unexpected {case} refusal: {error}"
        );
        assert!(old_record.is_file(), "{case} predecessor must survive");
        assert!(
            server
                .received_requests()
                .await
                .is_none_or(|requests| requests.is_empty()),
            "{case} must not download"
        );
    }
}

#[tokio::test]
async fn replacement_refuses_approved_root_targets_before_download() {
    for root_name in ["home", "prefix", "appdir"] {
        let server = MockServer::start().await;
        let fixture = Fixture::new().macos();
        let appdir = fixture.env.home.join("Applications");
        let target = match root_name {
            "home" => fixture.env.home.clone(),
            "prefix" => fixture.env.prefix.clone(),
            "appdir" => appdir.clone(),
            _ => unreachable!(),
        };
        let token = format!("root-{root_name}");
        seed_record(
            &fixture,
            &token,
            "0.9",
            appdir.as_str(),
            &[json!({"kind": "path", "target": target.as_str()})],
        );
        std::fs::create_dir_all(&fixture.env.home).expect("home");
        let marker = fixture.env.home.join(format!("{root_name}-marker"));
        std::fs::write(&marker, b"keep").expect("marker");
        let value = cask(
            &token,
            &format!("{}/unused.tar.gz", server.uri()),
            "no_check",
            vec![json!({"binary": ["tool"]})],
        );
        let (ctx, _reporter) =
            fixture.context_casks(vec![value], Arc::new(PanicRunner), reqwest::Client::new());
        let error = err(install::run(&ctx, install_args(&[&token], &appdir, true)).await);
        assert!(
            error.to_string().contains("must be below"),
            "unexpected {root_name} refusal: {error}"
        );
        assert_eq!(std::fs::read(marker).expect("marker survives"), b"keep");
        assert!(
            server
                .received_requests()
                .await
                .is_none_or(|requests| requests.is_empty()),
            "{root_name} equality must not download"
        );
    }
}
#[tokio::test]
async fn linux_uninstall_still_refuses() {
    let fixture = Fixture::new();
    let (ctx, _reporter) =
        fixture.context_casks(vec![], Arc::new(PanicRunner), reqwest::Client::new());
    let uninstall = uninstall::run(
        &ctx,
        uninstall::Args {
            tokens: vec!["foo".to_owned()],
            zap: false,
        },
    )
    .await;
    assert!(
        matches!(err(uninstall), OpError::Refusal { message } if message == "Casks are not supported on Linux.")
    );
}

#[tokio::test]
async fn linux_missing_cask_refuses() {
    let fixture = Fixture::new();
    let (ctx, _reporter) =
        fixture.context_casks(vec![], Arc::new(PanicRunner), reqwest::Client::new());
    let result = install::run(
        &ctx,
        install_args(&["ghost"], &fixture.env.home.join("Applications"), false),
    )
    .await;
    assert!(
        matches!(err(result), OpError::Refusal { message } if message == "Cask 'ghost' is unavailable.")
    );
}

#[tokio::test]
async fn linux_install_refuses_macos_only_before_io() {
    let fixture = Fixture::new();
    let appdir = fixture.env.home.join("Applications");
    for (kind, artifact) in [
        ("app", json!({"app": ["Foo.app"]})),
        ("suite", json!({"suite": ["Bar"]})),
        ("pkg", json!({"pkg": ["Foo.pkg"]})),
        ("service", json!({"service": ["Foo.service"]})),
        ("colorpicker", json!({"colorpicker": ["Foo.colorpicker"]})),
        ("prefpane", json!({"prefpane": ["Foo.prefPane"]})),
        ("dictionary", json!({"dictionary": ["Foo.dictionary"]})),
        ("input_method", json!({"input_method": ["Foo.app"]})),
        (
            "internet_plugin",
            json!({"internet_plugin": ["Foo.plugin"]}),
        ),
        (
            "keyboard_layout",
            json!({"keyboard_layout": ["Foo.bundle"]}),
        ),
        ("qlplugin", json!({"qlplugin": ["Foo.qlgenerator"]})),
        ("mdimporter", json!({"mdimporter": ["Foo.mdimporter"]})),
        ("screen_saver", json!({"screen_saver": ["Foo.saver"]})),
        (
            "audio_unit_plugin",
            json!({"audio_unit_plugin": ["Foo.component"]}),
        ),
        ("vst_plugin", json!({"vst_plugin": ["Foo.vst"]})),
        ("vst3_plugin", json!({"vst3_plugin": ["Foo.vst3"]})),
    ] {
        let value = cask(
            "maconly",
            "https://example.test/App.tar.gz",
            "no_check",
            vec![artifact],
        );
        let (ctx, _reporter) =
            fixture.context_casks(vec![value], Arc::new(PanicRunner), reqwest::Client::new());
        let result = install::run(&ctx, install_args(&["maconly"], &appdir, false)).await;
        let error = err(result);
        assert!(
            matches!(&error, OpError::Refusal { message } if message == "maconly: This cask requires macOS."),
            "{kind}: expected requires-macOS, got {error:?}"
        );
        assert!(
            !fixture.env.caskroom.join(".staging").exists(),
            "{kind}: staging created"
        );
    }
}

#[tokio::test]
async fn linux_install_refuses_macos_dependency_before_io() {
    let fixture = Fixture::new();
    let appdir = fixture.env.home.join("Applications");
    let mut value = cask(
        "macdep",
        "https://example.test/App.tar.gz",
        "no_check",
        vec![json!({"binary": ["tool"]})],
    );
    value["depends_on"] = json!({"macos": {">=": ["10.15"]}});
    let (ctx, _reporter) =
        fixture.context_casks(vec![value], Arc::new(PanicRunner), reqwest::Client::new());
    let result = install::run(&ctx, install_args(&["macdep"], &appdir, false)).await;
    let error = err(result);
    assert!(
        matches!(&error, OpError::Refusal { message } if message == "macdep: This cask requires macOS."),
        "expected requires-macOS for depends_on.macos, got {error:?}"
    );
    assert!(!fixture.env.caskroom.join(".staging").exists());
}

#[tokio::test]
async fn linux_install_refuses_execution_artifacts_before_io() {
    let fixture = Fixture::new();
    let appdir = fixture.env.home.join("Applications");
    for (label, artifact, expected_kind) in [
        (
            "installer script",
            json!({"installer": [{"script": {"executable": "install.sh"}}]}),
            "installer",
        ),
        (
            "installer manual",
            json!({"installer": [{"manual": "Caffeine.app"}]}),
            "",
        ),
        ("preflight", json!({"preflight": null}), "preflight"),
        ("postflight", json!({"postflight": null}), "postflight"),
        (
            "uninstall_preflight",
            json!({"uninstall_preflight": null}),
            "uninstall_preflight",
        ),
        (
            "uninstall_postflight",
            json!({"uninstall_postflight": null}),
            "uninstall_postflight",
        ),
        (
            "preflight_steps",
            json!({"preflight_steps": [{"steps": []}]}),
            "preflight_steps",
        ),
        (
            "postflight_steps",
            json!({"postflight_steps": [{"steps": []}]}),
            "postflight_steps",
        ),
        (
            "uninstall_preflight_steps",
            json!({"uninstall_preflight_steps": [{"steps": []}]}),
            "uninstall_preflight_steps",
        ),
        (
            "uninstall_postflight_steps",
            json!({"uninstall_postflight_steps": [{"steps": []}]}),
            "uninstall_postflight_steps",
        ),
    ] {
        let token = "danger";
        let value = cask(
            token,
            "https://example.test/App.tar.gz",
            "no_check",
            vec![artifact],
        );
        let (ctx, _reporter) =
            fixture.context_casks(vec![value], Arc::new(PanicRunner), reqwest::Client::new());
        let result = install::run(&ctx, install_args(&[token], &appdir, false)).await;
        let error = err(result);
        if expected_kind.is_empty() {
            assert!(
                matches!(&error, OpError::Refusal { message } if message == "danger: This cask requires macOS."),
                "{label}: expected requires-macOS, got {error:?}"
            );
        } else {
            let want = format!("Cask 'danger' uses unsupported artifact '{expected_kind}'.");
            assert!(
                matches!(&error, OpError::Refusal { message } if message == &want),
                "{label}: expected {want}, got {error:?}"
            );
        }
        assert!(
            !fixture.env.caskroom.join(".staging").exists(),
            "{label}: staging created"
        );
    }
}

#[tokio::test]
async fn linux_install_refuses_mixed_cask_before_io() {
    let fixture = Fixture::new();
    let appdir = fixture.env.home.join("Applications");
    let value = cask(
        "mixed",
        "https://example.test/App.tar.gz",
        "no_check",
        vec![json!({"binary": ["tool"]}), json!({"app": ["Foo.app"]})],
    );
    let (ctx, _reporter) =
        fixture.context_casks(vec![value], Arc::new(PanicRunner), reqwest::Client::new());
    let result = install::run(&ctx, install_args(&["mixed"], &appdir, false)).await;
    let error = err(result);
    assert!(
        matches!(&error, OpError::Refusal { message } if message == "mixed: This cask requires macOS."),
        "expected requires-macOS for mixed cask, got {error:?}"
    );
    assert!(!fixture.env.caskroom.join(".staging").exists());
}

#[tokio::test]
async fn linux_install_refuses_malformed_multiple_directives() {
    let fixture = Fixture::new();
    let appdir = fixture.env.home.join("Applications");
    let value = cask(
        "malformed",
        "https://example.test/App.tar.gz",
        "no_check",
        vec![json!({"binary": ["tool"], "app": ["Foo.app"]})],
    );
    let (ctx, _reporter) =
        fixture.context_casks(vec![value], Arc::new(PanicRunner), reqwest::Client::new());
    let result = install::run(&ctx, install_args(&["malformed"], &appdir, false)).await;
    let error = err(result);
    assert!(
        matches!(&error, OpError::Refusal { message } if message == "Cask 'malformed' artifact declares multiple directives; exactly one is required."),
        "expected malformed message, got {error:?}"
    );
    assert!(!fixture.env.caskroom.join(".staging").exists());
}

#[tokio::test]
async fn linux_install_refuses_unknown_artifact() {
    let fixture = Fixture::new();
    let appdir = fixture.env.home.join("Applications");
    let value = cask(
        "unknown",
        "https://example.test/App.tar.gz",
        "no_check",
        vec![json!({"future_kind": ["x"]})],
    );
    let (ctx, _reporter) =
        fixture.context_casks(vec![value], Arc::new(PanicRunner), reqwest::Client::new());
    let result = install::run(&ctx, install_args(&["unknown"], &appdir, false)).await;
    let error = err(result);
    assert!(
        matches!(&error, OpError::Refusal { message } if message == "Cask 'unknown' uses unsupported artifact 'future_kind'."),
        "expected unknown message, got {error:?}"
    );
    assert!(!fixture.env.caskroom.join(".staging").exists());
}

#[tokio::test]
async fn linux_install_refuses_dmg_before_download() {
    let server = MockServer::start().await;
    let body = tar_gz(&[("bin/tool", b"tool")]);
    let sha = sha_hex(&body);
    let url = serve(&server, "/App.dmg", body).await;
    let value = cask("disky", &url, &sha, vec![json!({"binary": ["tool"]})]);

    let fixture = Fixture::new();
    let appdir = fixture.env.home.join("Applications");
    let (ctx, _reporter) =
        fixture.context_casks(vec![value], Arc::new(PanicRunner), reqwest::Client::new());
    let result = install::run(&ctx, install_args(&["disky"], &appdir, false)).await;
    let error = err(result);
    assert!(
        matches!(&error, OpError::Refusal { message } if message == "Cask 'disky' ships a macOS disk image, which is unavailable on Linux."),
        "expected .dmg message, got {error:?}"
    );
    assert!(!fixture.env.caskroom.join(".staging").exists());
    assert!(
        server
            .received_requests()
            .await
            .is_none_or(|requests| requests.is_empty()),
        "no download may be attempted"
    );
}

#[tokio::test]
async fn linux_install_refuses_malformed_stage_only() {
    let fixture = Fixture::new();
    let appdir = fixture.env.home.join("Applications");
    for artifact in [
        json!({"stage_only": [false]}),
        json!({"stage_only": ["false"]}),
    ] {
        let value = cask(
            "badstage",
            "https://example.test/App.tar.gz",
            "no_check",
            vec![artifact],
        );
        let (ctx, _reporter) =
            fixture.context_casks(vec![value], Arc::new(PanicRunner), reqwest::Client::new());
        let result = install::run(&ctx, install_args(&["badstage"], &appdir, false)).await;
        let error = err(result);
        assert!(
            matches!(&error, OpError::Refusal { message } if message == "Cask 'badstage' uses unsupported artifact 'stage_only'."),
            "expected stage_only refusal, got {error:?}"
        );
        assert!(!fixture.env.caskroom.join(".staging").exists());
    }
}

#[tokio::test]
async fn linux_installs_reversible_artifacts() {
    let server = MockServer::start().await;
    let body = tar_gz(&[
        ("bin/tool", b"#!/bin/sh\n"),
        ("man/tool.1", b".TH tool 1\n"),
        ("bin/Foo.AppImage", b"appimage"),
        ("Fancy.font", b"font"),
        ("_tool", b"compdef"),
        ("extra.txt", b"extra"),
        ("bash-tool", b"complete"),
        ("fish-tool", b"complete"),
    ]);
    let sha = sha_hex(&body);
    let url = serve(&server, "/Bundle.tar.gz", body).await;

    let fixture = Fixture::new();
    let appdir = fixture.env.home.join("Applications");
    let value = cask(
        "bundle",
        &url,
        &sha,
        vec![
            json!({"binary": ["bin/tool"], "target": "tool"}),
            json!({"manpage": ["man/tool.1"]}),
            json!({"appimage": ["bin/Foo.AppImage"], "target": "Foo"}),
            json!({"font": ["Fancy.font"]}),
            json!({"zsh_completion": ["_tool"]}),
            json!({"bash_completion": ["bash-tool"]}),
            json!({"fish_completion": ["fish-tool"]}),
            json!({"artifact": ["extra.txt"], "target": format!("{}/extra.txt", appdir)}),
        ],
    );
    let (ctx, _reporter) =
        fixture.context_casks(vec![value], Arc::new(PanicRunner), reqwest::Client::new());
    install::run(&ctx, install_args(&["bundle"], &appdir, false))
        .await
        .expect("linux reversible install");

    let prefix = &fixture.env.prefix;
    let home = &fixture.env.home;
    assert!(
        std::fs::symlink_metadata(prefix.join("bin/tool").as_std_path())
            .expect("bin")
            .file_type()
            .is_symlink()
    );
    assert!(
        std::fs::symlink_metadata(prefix.join("share/man/man1/tool.1").as_std_path())
            .expect("man")
            .file_type()
            .is_symlink()
    );
    assert!(
        std::fs::symlink_metadata(home.join("Applications/Foo").as_std_path())
            .expect("appimage")
            .file_type()
            .is_symlink()
    );
    assert!(home.join("Library/Fonts/Fancy.font").is_file());
    assert!(prefix.join("share/zsh/site-functions/_tool").is_file());
    assert!(prefix.join("etc/bash_completion.d/bash-tool").is_file());
    assert!(
        prefix
            .join("share/fish/vendor_completions.d/fish-tool")
            .is_file()
    );
    assert!(appdir.join("extra.txt").is_file());
    assert!(fixture.env.caskroom.join("bundle/1.0").is_dir());
    let staging = fixture.env.caskroom.join(".staging");
    assert_eq!(
        std::fs::read_dir(&staging).map(|e| e.count()).unwrap_or(0),
        0,
        "staging empty after success"
    );
}

#[tokio::test]
async fn linux_stage_only_stages_without_deploy() {
    let server = MockServer::start().await;
    let body = tar_gz(&[("bin/tool", b"tool")]);
    let sha = sha_hex(&body);
    let url = serve(&server, "/Stage.tar.gz", body).await;
    let value = cask("stagey", &url, &sha, vec![json!({"stage_only": [true]})]);

    let fixture = Fixture::new();
    let appdir = fixture.env.home.join("Applications");
    let (ctx, _reporter) =
        fixture.context_casks(vec![value], Arc::new(PanicRunner), reqwest::Client::new());
    install::run(&ctx, install_args(&["stagey"], &appdir, false))
        .await
        .expect("stage_only install");

    assert!(!fixture.env.prefix.join("bin/tool").exists());
    assert!(fixture.env.caskroom.join("stagey/1.0").is_dir());
    assert!(
        fixture
            .env
            .caskroom
            .join("stagey/1.0/.zapbrew-record.json")
            .is_file()
    );
    let staging = fixture.env.caskroom.join(".staging");
    assert_eq!(
        std::fs::read_dir(&staging).map(|e| e.count()).unwrap_or(0),
        0,
        "staging empty after stage_only"
    );
}

#[tokio::test]
async fn linux_rollback_restores_on_action_failure() {
    let server = MockServer::start().await;
    let body = tar_gz(&[("bin/tool", b"tool"), ("man/tool.1", b".TH tool 1\n")]);
    let sha = sha_hex(&body);
    let url = serve(&server, "/Roll.tar.gz", body).await;
    let value = cask(
        "roll",
        &url,
        &sha,
        vec![
            json!({"binary": ["bin/tool"]}),
            json!({"manpage": ["man/tool.1"]}),
        ],
    );

    let fixture = Fixture::new();
    let appdir = fixture.env.home.join("Applications");
    let man_target = fixture.env.prefix.join("share/man/man1/tool.1");
    std::fs::create_dir_all(man_target.parent().expect("man target parent")).expect("man parent");
    std::fs::write(&man_target, b"blocker").expect("blocker");

    let (ctx, _reporter) =
        fixture.context_casks(vec![value], Arc::new(PanicRunner), reqwest::Client::new());
    let result = install::run(&ctx, install_args(&["roll"], &appdir, false)).await;
    assert!(result.is_err(), "expected apply to fail");

    // The first symlink was rolled back.
    assert!(!fixture.env.prefix.join("bin/tool").exists());
    // The pre-existing man file was left untouched.
    assert_eq!(std::fs::read(&man_target).expect("man"), b"blocker");
    let staging = fixture.env.caskroom.join(".staging");
    assert_eq!(
        std::fs::read_dir(&staging).map(|e| e.count()).unwrap_or(0),
        0,
        "staging drained after rollback"
    );
}

#[tokio::test]
async fn linux_rollback_removes_newly_created_parent_tree() {
    let server = MockServer::start().await;
    let body = tar_gz(&[("first", b"one"), ("second", b"two")]);
    let sha = sha_hex(&body);
    let url = serve(&server, "/Parents.tar.gz", body).await;
    let fixture = Fixture::new();
    let ssh = fixture.env.home.join(".ssh");
    fs::create_dir_all(&ssh).expect("ssh");
    fs::write(ssh.join("sentinel"), b"keep").expect("sentinel");
    let authorized_keys = ssh.join("authorized_keys");
    let child = authorized_keys.join("child");
    let value = cask(
        "parents",
        &url,
        &sha,
        vec![
            json!({"artifact": ["first"], "target": child.as_str()}),
            json!({"artifact": ["second"], "target": authorized_keys.as_str()}),
        ],
    );
    let appdir = fixture.env.home.join("Applications");
    let (ctx, _reporter) =
        fixture.context_casks(vec![value], Arc::new(PanicRunner), reqwest::Client::new());

    let result = install::run(&ctx, install_args(&["parents"], &appdir, false)).await;
    assert!(
        result.is_err(),
        "second target must collide with created parent"
    );
    assert!(ssh.is_dir(), "pre-existing parent must survive");
    assert_eq!(fs::read(ssh.join("sentinel")).expect("sentinel"), b"keep");
    assert!(
        !authorized_keys.exists(),
        "rollback must remove the parent tree created by the first action"
    );
}

#[tokio::test]
async fn linux_artifact_target_outside_roots_preflights() {
    let fixture = Fixture::new();
    let appdir = fixture.env.home.join("Applications");
    let value = cask(
        "escape",
        "https://example.test/App.tar.gz",
        "no_check",
        vec![json!({"artifact": ["payload.txt"], "target": "/etc/evil.txt"})],
    );
    let (ctx, _reporter) =
        fixture.context_casks(vec![value], Arc::new(PanicRunner), reqwest::Client::new());
    let result = install::run(&ctx, install_args(&["escape"], &appdir, false)).await;
    let error = err(result);
    assert!(
        matches!(&error, OpError::Refusal { message } if message.contains("outside approved roots")),
        "expected outside-roots refusal, got {error}"
    );
    assert!(!fixture.env.caskroom.join(".staging").exists());
}

#[tokio::test]
async fn linux_artifact_unknown_top_level_key_refuses() {
    let fixture = Fixture::new();
    let appdir = fixture.env.home.join("Applications");
    let value = cask(
        "evil-top",
        "https://example.test/App.tar.gz",
        "no_check",
        vec![json!({"binary": ["tool"], "target": "tool", "evil": "x"})],
    );
    let (ctx, _reporter) =
        fixture.context_casks(vec![value], Arc::new(PanicRunner), reqwest::Client::new());
    let result = install::run(&ctx, install_args(&["evil-top"], &appdir, false)).await;
    let error = err(result);
    assert!(
        matches!(&error, OpError::Refusal { message } if message.contains("unknown key")),
        "expected unknown top-level key refusal, got {error}"
    );
    assert!(!fixture.env.caskroom.join(".staging").exists());
}

#[tokio::test]
async fn linux_artifact_unknown_nested_option_key_refuses() {
    let fixture = Fixture::new();
    let appdir = fixture.env.home.join("Applications");
    let value = cask(
        "evil-nested",
        "https://example.test/App.tar.gz",
        "no_check",
        vec![json!({"binary": ["tool", {"target": "tool", "evil": "x"}]})],
    );
    let (ctx, _reporter) =
        fixture.context_casks(vec![value], Arc::new(PanicRunner), reqwest::Client::new());
    let result = install::run(&ctx, install_args(&["evil-nested"], &appdir, false)).await;
    let error = err(result);
    assert!(
        matches!(&error, OpError::Refusal { message } if message.contains("unknown option")),
        "expected unknown nested option refusal, got {error}"
    );
    assert!(!fixture.env.caskroom.join(".staging").exists());
}

#[tokio::test]
async fn linux_stage_only_string_true_refuses() {
    let fixture = Fixture::new();
    let appdir = fixture.env.home.join("Applications");
    let value = cask(
        "strue",
        "https://example.test/App.tar.gz",
        "no_check",
        vec![json!({"stage_only": ["true"]})],
    );
    let (ctx, _reporter) =
        fixture.context_casks(vec![value], Arc::new(PanicRunner), reqwest::Client::new());
    let result = install::run(&ctx, install_args(&["strue"], &appdir, false)).await;
    let error = err(result);
    assert!(
        matches!(&error, OpError::Refusal { message } if message == "Cask 'strue' uses unsupported artifact 'stage_only'."),
        "expected stage_only string-true refusal, got {error}"
    );
    assert!(!fixture.env.caskroom.join(".staging").exists());
}

#[tokio::test]
async fn missing_cask_refuses() {
    let fixture = Fixture::new().macos();
    let (ctx, _reporter) =
        fixture.context_casks(vec![], Arc::new(PanicRunner), reqwest::Client::new());
    let result = install::run(
        &ctx,
        install_args(&["ghost"], Utf8Path::new("/tmp/apps"), false),
    )
    .await;
    assert!(
        matches!(err(result), OpError::Refusal { message } if message == "Cask 'ghost' is unavailable.")
    );
}

#[tokio::test]
async fn disabled_cask_refuses_before_io() {
    let fixture = Fixture::new();
    let appdir = fixture.env.home.join("Applications");
    let mut value = cask(
        "retired",
        "https://example.test/App.tar.gz",
        "no_check",
        vec![json!({"binary": ["tool"]})],
    );
    value["disabled"] = json!(true);
    value["disable_reason"] = json!("is discontinued upstream");
    let (ctx, _reporter) =
        fixture.context_casks(vec![value], Arc::new(PanicRunner), reqwest::Client::new());

    let result = install::run(&ctx, install_args(&["retired"], &appdir, false)).await;
    assert!(
        matches!(err(result), OpError::Refusal { message } if message == "retired has been disabled because it is discontinued upstream!")
    );
    assert!(!fixture.env.caskroom.join(".staging").exists());
    assert!(!fixture.env.locks.exists());
}

#[tokio::test]
async fn old_token_rename_resolves() {
    let server = MockServer::start().await;
    let body = tar_gz(&[("Everything.app/run", b"bin")]);
    let sha = sha_hex(&body);
    let url = serve(&server, "/Everything.tar.gz", body).await;
    let mut value = cask(
        "everything",
        &url,
        &sha,
        vec![json!({"app": ["Everything.app"]})],
    );
    value["old_tokens"] = json!(["every-thing"]);

    let fixture = Fixture::new().macos();
    let appdir = fixture.env.home.join("Applications");
    let (ctx, _reporter) =
        fixture.context_casks(vec![value], Arc::new(PanicRunner), reqwest::Client::new());
    install::run(&ctx, install_args(&["every-thing"], &appdir, false))
        .await
        .expect("rename install");
    assert!(appdir.join("Everything.app").is_dir());
}

#[tokio::test]
async fn unsupported_artifact_preflights_before_io() {
    let fixture = Fixture::new().macos();
    let value = cask(
        "scripted",
        "https://example.test/App.zip",
        "no_check",
        vec![json!({"installer": [{"script": {"executable": "install.sh"}}]})],
    );
    let (ctx, _reporter) =
        fixture.context_casks(vec![value], Arc::new(PanicRunner), reqwest::Client::new());
    let result = install::run(
        &ctx,
        install_args(&["scripted"], &fixture.env.home.join("Applications"), false),
    )
    .await;
    assert!(
        matches!(err(result), OpError::Refusal { message } if message == "Cask 'scripted' uses unsupported artifact 'installer'.")
    );
    // No download or staging happened.
    assert!(!fixture.env.caskroom.join(".staging").exists());
}

#[tokio::test]
async fn macos_refuses_linux_only_appimage_and_manual_installer_before_io() {
    let fixture = Fixture::new().macos();
    let appdir = fixture.env.home.join("Applications");
    for (token, artifact, kind) in [
        (
            "linux-image",
            json!({"appimage": ["Tool.AppImage"]}),
            "appimage",
        ),
        (
            "manual-installer",
            json!({"installer": [{"manual": "Tool.app"}]}),
            "installer",
        ),
    ] {
        let value = cask(
            token,
            "https://example.test/App.tar.gz",
            "no_check",
            vec![artifact],
        );
        let (ctx, _reporter) =
            fixture.context_casks(vec![value], Arc::new(PanicRunner), reqwest::Client::new());
        let result = install::run(&ctx, install_args(&[token], &appdir, false)).await;
        let expected = format!("Cask '{token}' uses unsupported artifact '{kind}'.");
        assert!(
            matches!(err(result), OpError::Refusal { message } if message == expected),
            "{token}: expected {expected}"
        );
        assert!(!fixture.env.caskroom.join(".staging").exists());
    }
}

#[tokio::test]
async fn explicit_target_outside_approved_roots_preflights_before_io() {
    let fixture = Fixture::new().macos();
    for artifacts in [
        vec![json!({"artifact": ["payload.txt", {"target": "/etc/evil.txt"}]})],
        vec![json!({"app": ["Demo.app", {"target": "/opt/evil/Demo.app"}]})],
    ] {
        let value = cask(
            "escape",
            "https://example.test/App.zip",
            "no_check",
            artifacts,
        );
        let (ctx, _reporter) =
            fixture.context_casks(vec![value], Arc::new(PanicRunner), reqwest::Client::new());
        let result = install::run(
            &ctx,
            install_args(&["escape"], &fixture.env.home.join("Applications"), false),
        )
        .await;
        let error = err(result);
        assert!(
            matches!(&error, OpError::Refusal { message } if message.contains("outside approved roots")),
            "expected outside-roots refusal, got {error}"
        );
        // No download or staging happened.
        assert!(!fixture.env.caskroom.join(".staging").exists());
    }
}

#[tokio::test]
async fn explicit_target_inside_caskroom_preflights_before_io() {
    let fixture = Fixture::new();
    let appdir = fixture.env.home.join("Applications");
    let target = fixture.env.caskroom.join("owned/1.0");
    let value = cask(
        "internal-target",
        "https://example.test/App.tar.gz",
        "no_check",
        vec![json!({"artifact": ["payload"], "target": target.as_str()})],
    );
    let (ctx, _reporter) =
        fixture.context_casks(vec![value], Arc::new(PanicRunner), reqwest::Client::new());

    let result = install::run(&ctx, install_args(&["internal-target"], &appdir, false)).await;
    assert!(
        matches!(&result, Err(OpError::Refusal { message }) if message.contains("managed Caskroom")),
        "expected Caskroom target refusal, got {result:?}"
    );
    assert!(!fixture.env.caskroom.join(".staging").exists());
    assert!(!fixture.env.locks.exists());
}

#[tokio::test]
async fn zip_and_tar_and_bare_extract_place_app() {
    for (route, body) in [
        ("/App.zip", zip_bytes(&[("Config.app/Contents/info", b"x")])),
        ("/App.tar.gz", tar_gz(&[("Config.app/Contents/info", b"x")])),
    ] {
        let server = MockServer::start().await;
        let sha = sha_hex(&body);
        let url = serve(&server, route, body).await;
        let value = cask("config", &url, &sha, vec![json!({"app": ["Config.app"]})]);
        let fixture = Fixture::new().macos();
        let appdir = fixture.env.home.join("Applications");
        let (ctx, _reporter) =
            fixture.context_casks(vec![value], Arc::new(PanicRunner), reqwest::Client::new());
        install::run(&ctx, install_args(&["config"], &appdir, false))
            .await
            .expect("extract install");
        assert!(appdir.join("Config.app/Contents/info").is_file());
    }
}

#[tokio::test]
async fn tar_with_symlink_entry_preserves_link_without_following() {
    let fixture = Fixture::new().macos();
    let appdir = fixture.env.home.join("Applications");
    let server = MockServer::start().await;
    let body = tar_gz_with_symlink(
        ("Demo.app/Contents/info", b"x"),
        ("Demo.app/Contents/evil-link", "/etc/passwd"),
    );
    let url = serve(&server, "/App.tar.gz", body.clone()).await;
    let value = cask(
        "linky",
        &url,
        &sha_hex(&body),
        vec![json!({"app": ["Demo.app"]})],
    );
    let (ctx, _reporter) =
        fixture.context_casks(vec![value], Arc::new(PanicRunner), reqwest::Client::new());
    install::run(&ctx, install_args(&["linky"], &appdir, false))
        .await
        .expect("extract install with symlink");
    // The app is deployed (symlink was not followed during extraction).
    assert!(appdir.join("Demo.app/Contents/info").is_file());
    // The symlink is preserved as a symlink pointing at its original target.
    let link = appdir.join("Demo.app/Contents/evil-link");
    let meta = match fs::symlink_metadata(link.as_std_path()) {
        Ok(meta) => meta,
        Err(e) => panic!("symlink_metadata evil-link: {e}"),
    };
    assert!(meta.file_type().is_symlink(), "evil-link must be a symlink");
    let target = match fs::read_link(link.as_std_path()) {
        Ok(t) => t,
        Err(e) => panic!("read_link evil-link: {e}"),
    };
    assert_eq!(target, std::path::Path::new("/etc/passwd"));
}

#[tokio::test]
async fn staged_record_symlink_refuses_without_overwriting_external_file() {
    let server = MockServer::start().await;
    let fixture = Fixture::new();
    let sentinel = fixture.env.home.join("sentinel");
    fs::create_dir_all(&fixture.env.home).expect("home");
    fs::write(&sentinel, b"keep").expect("sentinel");
    let body = tar_gz_with_symlink(
        ("bin/tool", b"tool"),
        (".zapbrew-record.json", sentinel.as_str()),
    );
    let sha = sha_hex(&body);
    let url = serve(&server, "/Record.tar.gz", body).await;
    let value = cask(
        "record-link",
        &url,
        &sha,
        vec![json!({"binary": ["bin/tool"]})],
    );
    let appdir = fixture.env.home.join("Applications");
    let (ctx, _reporter) =
        fixture.context_casks(vec![value], Arc::new(PanicRunner), reqwest::Client::new());

    let result = install::run(&ctx, install_args(&["record-link"], &appdir, false)).await;
    assert!(
        matches!(&result, Err(OpError::Refusal { message }) if message.contains("symlink")),
        "expected record symlink refusal, got {result:?}"
    );
    assert_eq!(fs::read(&sentinel).expect("sentinel survives"), b"keep");
    assert!(!fixture.env.prefix.join("bin/tool").exists());
    assert_eq!(
        fs::read_dir(fixture.env.caskroom.join(".staging"))
            .expect("staging root")
            .count(),
        0
    );
}

#[tokio::test]
async fn dmg_extract_detaches_on_ditto_failure() {
    let server = MockServer::start().await;
    let sha = sha_hex(b"dmg-bytes");
    let url = serve(&server, "/App.dmg", b"dmg-bytes".to_vec()).await;
    let value = cask("mounted", &url, &sha, vec![json!({"app": ["Mounted.app"]})]);

    let fixture = Fixture::new().macos();
    let appdir = fixture.env.home.join("Applications");
    let mount = fixture.env.caskroom.join(".mnt");
    let plist = format!(
        "<?xml version=\"1.0\"?>\n<plist version=\"1.0\"><dict><key>system-entities</key><array><dict><key>mount-point</key><string>{mount}</string></dict></array></dict></plist>"
    );
    // attach ok (plist), ditto fails, detach ok.
    let runner = ScriptedRunner::with(vec![Ok(plist.into_bytes()), Err(()), Ok(Vec::new())]);
    let (ctx, _reporter) =
        fixture.context_casks(vec![value], runner.clone(), reqwest::Client::new());
    let result = install::run(&ctx, install_args(&["mounted"], &appdir, false)).await;
    assert!(result.is_err());
    let argv = runner.argv();
    assert_eq!(argv[0][0], "/usr/bin/hdiutil");
    assert_eq!(argv[0][1], "attach");
    assert_eq!(argv[1][0], "/usr/bin/ditto");
    assert_eq!(argv[2][0], "/usr/bin/hdiutil");
    assert_eq!(argv[2][1], "detach");
}

#[tokio::test]
async fn artifact_mappings_cover_appendix_h() {
    let server = MockServer::start().await;
    let body = tar_gz(&[
        ("Everything.app/run", b"app"),
        ("bin/tool", b"#!/bin/sh\n"),
        ("man/tool.1", b".TH tool 1\n"),
        ("Fancy.font", b"font"),
        ("Fancy.prefPane/data", b"pane"),
        ("_tool", b"compdef"),
        ("extra.txt", b"extra"),
    ]);
    let sha = sha_hex(&body);
    let url = serve(&server, "/Bundle.tar.gz", body).await;
    let fixture = Fixture::new().macos();
    let appdir = fixture.env.home.join("Applications");
    let value = cask(
        "bundle",
        &url,
        &sha,
        vec![
            json!({"app": ["Everything.app"]}),
            json!({"binary": ["bin/tool"], "target": "tool"}),
            json!({"manpage": ["man/tool.1"]}),
            json!({"font": ["Fancy.font"]}),
            json!({"prefpane": ["Fancy.prefPane"]}),
            json!({"zsh_completion": ["_tool"]}),
            json!({"artifact": ["extra.txt"], "target": format!("{}/extra.txt", appdir)}),
        ],
    );
    let (ctx, _reporter) =
        fixture.context_casks(vec![value], Arc::new(PanicRunner), reqwest::Client::new());
    install::run(&ctx, install_args(&["bundle"], &appdir, false))
        .await
        .expect("appendix install");

    let prefix = &fixture.env.prefix;
    let home = &fixture.env.home;
    assert!(appdir.join("Everything.app/run").is_file());
    assert!(
        std::fs::symlink_metadata(prefix.join("bin/tool"))
            .expect("bin")
            .file_type()
            .is_symlink()
    );
    assert!(
        std::fs::symlink_metadata(prefix.join("share/man/man1/tool.1"))
            .expect("man")
            .file_type()
            .is_symlink()
    );
    assert!(home.join("Library/Fonts/Fancy.font").is_file());
    assert!(
        home.join("Library/PreferencePanes/Fancy.prefPane/data")
            .is_file()
    );
    assert!(prefix.join("share/zsh/site-functions/_tool").is_file());
    assert!(appdir.join("extra.txt").is_file());
}

#[tokio::test]
async fn pkg_rollback_reports_irreversible_leftover() {
    let server = MockServer::start().await;
    let body = tar_gz(&[("App.pkg", b"pkg"), ("bad.app", b"app")]);
    let sha = sha_hex(&body);
    let url = serve(&server, "/Pkg.tar.gz", body).await;
    let fixture = Fixture::new().macos();
    // app target parent is a file, so the app move fails after the pkg installs.
    let appdir = fixture.env.home.join("Applications");
    std::fs::create_dir_all(&appdir).expect("appdir");
    std::fs::write(appdir.join("bad.app"), b"blocker").expect("blocker");
    let value = cask(
        "pkgcask",
        &url,
        &sha,
        vec![json!({"pkg": ["App.pkg"]}), json!({"app": ["bad.app"]})],
    );
    let runner = ScriptedRunner::with(vec![Ok(Vec::new())]);
    let (ctx, _reporter) =
        fixture.context_casks(vec![value], runner.clone(), reqwest::Client::new());
    let result = install::run(&ctx, install_args(&["pkgcask"], &appdir, false)).await;
    match result {
        Err(OpError::RollbackIncomplete { leftovers, .. }) => {
            assert!(
                leftovers.contains("pkg installed"),
                "leftovers: {leftovers}"
            );
        }
        other => panic!("expected RollbackIncomplete, got {other:?}"),
    }
    assert_eq!(runner.argv()[0][0], "/usr/sbin/installer");
}

#[tokio::test]
async fn receipt_written_then_already_installed_noops() {
    let server = MockServer::start().await;
    let body = tar_gz(&[("Solo.app/run", b"app")]);
    let sha = sha_hex(&body);
    let url = serve(&server, "/Solo.tar.gz", body).await;
    let value = cask("solo", &url, &sha, vec![json!({"app": ["Solo.app"]})]);
    let fixture = Fixture::new().macos();
    let appdir = fixture.env.home.join("Applications");
    let (ctx, reporter) =
        fixture.context_casks(vec![value], Arc::new(PanicRunner), reqwest::Client::new());
    install::run(&ctx, install_args(&["solo"], &appdir, false))
        .await
        .expect("first install");

    let meta = fixture.env.caskroom.join("solo/.metadata/1.0");
    let entries = fingerprint(&meta);
    assert!(
        entries
            .iter()
            .any(|entry| entry.ends_with("Casks/solo.json") || entry.contains("Casks")),
        "missing receipt: {entries:?}"
    );
    let receipt = find_receipt(&meta);
    let bytes = std::fs::read(&receipt).expect("receipt");
    let raw: Value = serde_json::from_slice(&bytes).expect("json");
    assert_eq!(raw["token"], "solo");
    // Byte-identity: the raw receipt stays exactly pretty JSON + trailing newline
    // for brew interop, unchanged by the typed-record rebuild.
    let mut expected = serde_json::to_vec_pretty(&raw).expect("reserialize");
    expected.push(b'\n');
    assert_eq!(bytes, expected, "raw receipt must be byte-identical");

    reporter.take();
    install::run(&ctx, install_args(&["solo"], &appdir, false))
        .await
        .expect("second install no-ops");
    let messages = reporter.take();
    assert!(
        messages.iter().any(|m| m.contains("already installed")),
        "expected already-installed warning, got {messages:?}"
    );
}

fn find_receipt(meta: &Utf8Path) -> Utf8PathBuf {
    for entry in std::fs::read_dir(meta).expect("meta dir") {
        let path = Utf8PathBuf::from_path_buf(entry.expect("entry").path()).expect("utf8");
        let receipt = path.join("Casks/solo.json");
        if receipt.is_file() {
            return receipt;
        }
    }
    panic!("no receipt under {meta}");
}

#[tokio::test]
async fn failed_promotion_removes_receipt_owned_directories() {
    let server = MockServer::start().await;
    let body = tar_gz(&[("bin/tool", b"tool")]);
    let sha = sha_hex(&body);
    let url = serve(&server, "/Blocked.tar.gz", body).await;
    let fixture = Fixture::new();
    let token_dir = fixture.env.caskroom.join("blocked");
    fs::create_dir_all(&token_dir).expect("token dir");
    let blocker = token_dir.join("1.0");
    fs::write(&blocker, b"blocker").expect("version blocker");
    let value = cask("blocked", &url, &sha, vec![json!({"binary": ["bin/tool"]})]);
    let appdir = fixture.env.home.join("Applications");
    let (ctx, _reporter) =
        fixture.context_casks(vec![value], Arc::new(PanicRunner), reqwest::Client::new());

    let result = install::run(&ctx, install_args(&["blocked"], &appdir, false)).await;
    assert!(
        result.is_err(),
        "promotion must refuse the regular-file version"
    );
    assert_eq!(fs::read(&blocker).expect("blocker survives"), b"blocker");
    assert!(
        !token_dir.join(".metadata").exists(),
        "rollback must remove the receipt tree created by this install"
    );
    assert!(!fixture.env.prefix.join("bin/tool").exists());
}

#[tokio::test]
async fn force_reinstall_replaces_old_artifact_and_clears_backups() {
    let server = MockServer::start().await;
    let body = tar_gz(&[("Editor.app/run", b"content")]);
    let sha = sha_hex(&body);
    let url = serve(&server, "/Editor.tar.gz", body).await;
    let value = cask("editor", &url, &sha, vec![json!({"app": ["Editor.app"]})]);

    let fixture = Fixture::new().macos();
    let appdir = fixture.env.home.join("Applications");
    let (ctx, _reporter) =
        fixture.context_casks(vec![value], Arc::new(PanicRunner), reqwest::Client::new());

    install::run(&ctx, install_args(&["editor"], &appdir, false))
        .await
        .expect("first install");
    assert_eq!(
        std::fs::read(appdir.join("Editor.app/run")).expect("run"),
        b"content"
    );

    // Tamper with the deployed app to prove the reinstall replaces it wholesale.
    std::fs::write(appdir.join("Editor.app/run"), b"tampered").expect("tamper");
    std::fs::write(appdir.join("Editor.app/extra"), b"junk").expect("extra");

    install::run(&ctx, install_args(&["editor"], &appdir, true))
        .await
        .expect("force reinstall");

    assert_eq!(
        std::fs::read(appdir.join("Editor.app/run")).expect("run"),
        b"content"
    );
    assert!(!appdir.join("Editor.app/extra").exists());
    assert!(fixture.env.caskroom.join("editor/1.0").is_dir());
    let staging = fixture.env.caskroom.join(".staging");
    let leftovers = std::fs::read_dir(&staging)
        .map(|entries| entries.count())
        .unwrap_or(0);
    assert_eq!(leftovers, 0, "staging should be empty after success");
}

#[tokio::test]
async fn force_reinstall_failure_restores_old_artifact_and_version() {
    let server = MockServer::start().await;
    let body = tar_gz(&[
        ("Editor.app/run", b"new-editor"),
        ("Second.app/run", b"new-second"),
    ]);
    let sha = sha_hex(&body);
    let url = serve(&server, "/Two.tar.gz", body).await;
    let value = cask(
        "editor",
        &url,
        &sha,
        vec![
            json!({"app": ["Editor.app"]}),
            json!({"app": ["Second.app"]}),
        ],
    );

    let fixture = Fixture::new().macos();
    let appdir = fixture.env.home.join("Applications");
    std::fs::create_dir_all(&appdir).expect("appdir");

    // Manual prior install: receipt lists only the first app, so Second.app is an
    // unrelated target the new plan must not silently clobber.
    let old_receipt = json!({
        "token": "editor",
        "version": "1.0",
        "sha256": "no_check",
        "url": "https://example.test/old.zip",
        "artifacts": [{"app": ["Editor.app"]}]
    });
    let receipt_path = fixture
        .env
        .caskroom
        .join("editor/.metadata/1.0/20260101000000/Casks/editor.json");
    std::fs::create_dir_all(receipt_path.parent().expect("receipt parent")).expect("receipt dir");
    std::fs::write(
        &receipt_path,
        serde_json::to_vec_pretty(&old_receipt).expect("receipt json"),
    )
    .expect("write receipt");
    std::fs::create_dir_all(fixture.env.caskroom.join("editor/1.0")).expect("version dir");
    seed_record(
        &fixture,
        "editor",
        "1.0",
        appdir.as_str(),
        &[json!({"kind": "path", "target": appdir.join("Editor.app").to_string()})],
    );
    std::fs::create_dir_all(appdir.join("Editor.app")).expect("old app");
    std::fs::write(appdir.join("Editor.app/run"), b"old-editor").expect("old run");
    // Unrelated occupant at the second app's target: the reinstall must refuse it.
    std::fs::write(appdir.join("Second.app"), b"blocker").expect("blocker");

    let (ctx, _reporter) =
        fixture.context_casks(vec![value], Arc::new(PanicRunner), reqwest::Client::new());
    let result = install::run(&ctx, install_args(&["editor"], &appdir, true)).await;
    assert!(
        result.is_err(),
        "expected reinstall to refuse the collision"
    );

    // Old deployed artifact and old version/receipt are both restored.
    assert_eq!(
        std::fs::read(appdir.join("Editor.app/run")).expect("restored run"),
        b"old-editor"
    );
    assert_eq!(
        std::fs::read(appdir.join("Second.app")).expect("blocker"),
        b"blocker"
    );
    assert!(fixture.env.caskroom.join("editor/1.0").is_dir());
    assert!(receipt_path.is_file());
}

/// Scripts a DMG attach/ditto/detach where `ditto` plants an escaping symlink
/// `link -> link_target` into the staging directory it is handed.
struct DmgSymlinkRunner {
    calls: Mutex<Vec<Vec<String>>>,
    plist: String,
    link_target: Utf8PathBuf,
}

impl CommandRunner for DmgSymlinkRunner {
    fn run(&self, spec: &CommandSpec) -> Result<CommandOutput, std::io::Error> {
        let program = spec.program().to_string_lossy().into_owned();
        let args: Vec<String> = spec
            .arguments()
            .iter()
            .map(|a| a.to_string_lossy().into_owned())
            .collect();
        let mut call = vec![program.clone()];
        call.extend(args.clone());
        self.calls.lock().expect("calls").push(call);
        if program == "/usr/bin/hdiutil" && args.first().map(String::as_str) == Some("attach") {
            return Ok(CommandOutput::new(
                status(true),
                self.plist.clone().into_bytes(),
                Vec::new(),
            ));
        }
        if program == "/usr/bin/ditto" {
            // ditto <mount> <staging>: simulate extraction of an escaping symlink.
            let staging = Utf8Path::new(&args[1]);
            std::os::unix::fs::symlink(
                self.link_target.as_std_path(),
                staging.join("link").as_std_path(),
            )
            .expect("plant staged symlink");
        }
        Ok(CommandOutput::new(status(true), Vec::new(), Vec::new()))
    }
}

#[tokio::test]
async fn unapproved_appdir_refuses_before_any_io() {
    let server = MockServer::start().await;
    let body = b"never-downloaded".to_vec();
    let sha = sha_hex(&body);
    let url = serve(&server, "/Blocked.tar.gz", body).await;
    let value = cask("blocked", &url, &sha, vec![json!({"app": ["Blocked.app"]})]);
    let fixture = Fixture::new().macos();
    // An appdir outside {home, prefix, /Applications} would become a record
    // removal root broad enough to defeat target confinement; refuse up front.
    let outside = Utf8PathBuf::from("/opt/apps");
    let (ctx, _reporter) =
        fixture.context_casks(vec![value], Arc::new(PanicRunner), reqwest::Client::new());
    let result = install::run(&ctx, install_args(&["blocked"], &outside, false)).await;
    let error = err(result);
    assert!(
        matches!(&error, OpError::Refusal { message } if message.contains("appdir")),
        "expected appdir refusal, got {error:?}"
    );
    assert!(!fixture.env.caskroom.join("blocked").exists());
    assert!(
        server
            .received_requests()
            .await
            .is_none_or(|requests| requests.is_empty()),
        "no download may be attempted"
    );
}

#[tokio::test]
async fn dmg_source_through_staging_symlink_refuses_before_mutation() {
    let server = MockServer::start().await;
    let body = b"dmg-bytes".to_vec();
    let sha = sha_hex(&body);
    let url = serve(&server, "/App.dmg", body).await;
    let value = cask(
        "mounted",
        &url,
        &sha,
        vec![json!({"app": ["link/sentinel"]})],
    );

    let fixture = Fixture::new().macos();
    let appdir = fixture.env.home.join("Applications");
    // External tree the staged symlink points at; it must be untouched.
    let external = fixture.env.home.join("external");
    std::fs::create_dir_all(&external).expect("external dir");
    std::fs::write(external.join("sentinel"), b"external-sentinel").expect("sentinel");
    let before = fingerprint(&external);

    let mount = fixture.env.caskroom.join(".mnt");
    let plist = format!(
        "<?xml version=\"1.0\"?>\n<plist version=\"1.0\"><dict><key>system-entities</key><array><dict><key>mount-point</key><string>{mount}</string></dict></array></dict></plist>"
    );
    let runner = Arc::new(DmgSymlinkRunner {
        calls: Mutex::new(Vec::new()),
        plist,
        link_target: external.clone(),
    });
    let (ctx, _reporter) =
        fixture.context_casks(vec![value], runner.clone(), reqwest::Client::new());
    let result = install::run(&ctx, install_args(&["mounted"], &appdir, false)).await;

    let error = err(result);
    assert!(
        matches!(&error, OpError::Refusal { message } if message.contains("symlink")),
        "expected symlink refusal, got {error:?}"
    );
    // The declared target was never created; the external tree is unchanged.
    assert!(!appdir.join("sentinel").exists());
    assert_eq!(
        std::fs::read(external.join("sentinel")).expect("external survives"),
        b"external-sentinel"
    );
    assert_eq!(fingerprint(&external), before);
}

#[tokio::test]
async fn custom_appdir_uninstall_removes_stored_target_without_appdir_arg() {
    let server = MockServer::start().await;
    let body = tar_gz(&[("Custom.app/run", b"custom")]);
    let sha = sha_hex(&body);
    let url = serve(&server, "/Custom.tar.gz", body).await;
    let value = cask("custom", &url, &sha, vec![json!({"app": ["Custom.app"]})]);

    let fixture = Fixture::new().macos();
    // Install into a scratch appdir that is not /Applications.
    let appdir = fixture.env.home.join("Scratch/Applications");
    let (ctx, _reporter) =
        fixture.context_casks(vec![value], Arc::new(PanicRunner), reqwest::Client::new());
    install::run(&ctx, install_args(&["custom"], &appdir, false))
        .await
        .expect("install into custom appdir");
    assert!(
        appdir.join("Custom.app/run").is_file(),
        "app lands in custom appdir"
    );

    // Uninstall takes no appdir argument; it must read the install-time appdir
    // from the promoted state and remove the custom target, not /Applications.
    uninstall::run(
        &ctx,
        uninstall::Args {
            tokens: vec!["custom".to_owned()],
            zap: false,
        },
    )
    .await
    .expect("uninstall without appdir arg");
    assert!(
        !appdir.join("Custom.app").exists(),
        "custom-appdir target must be removed"
    );
    assert!(
        !fixture.env.caskroom.join("custom").exists(),
        "version tree must be removed"
    );
    assert!(
        !Utf8Path::new("/Applications/Custom.app").exists(),
        "/Applications must be untouched"
    );
}

#[tokio::test]
async fn force_reinstall_with_changed_appdir_uses_each_plan_own_appdir() {
    let server = MockServer::start().await;
    let first = tar_gz(&[("Mover.app/run", b"first")]);
    let first_sha = sha_hex(&first);
    let first_url = serve(&server, "/Mover1.tar.gz", first).await;
    let second = tar_gz(&[("Mover.app/run", b"second")]);
    let second_sha = sha_hex(&second);
    let second_url = serve(&server, "/Mover2.tar.gz", second).await;

    let fixture = Fixture::new().macos();
    let appdir_a = fixture.env.home.join("A/Applications");
    let appdir_b = fixture.env.home.join("B/Applications");

    // First install lands in appdir A.
    let (ctx_a, _r) = fixture.context_casks(
        vec![cask(
            "mover",
            &first_url,
            &first_sha,
            vec![json!({"app": ["Mover.app"]})],
        )],
        Arc::new(PanicRunner),
        reqwest::Client::new(),
    );
    install::run(&ctx_a, install_args(&["mover"], &appdir_a, false))
        .await
        .expect("first install");
    assert_eq!(
        std::fs::read(appdir_a.join("Mover.app/run")).expect("a run"),
        b"first"
    );

    // Force reinstall targets appdir B. The old plan is reconstructed against the
    // stored appdir A, so A's target is backed up and cleared; the new plan uses
    // B, so B receives the new artifact.
    let (ctx_b, _r2) = fixture.context_casks(
        vec![cask(
            "mover",
            &second_url,
            &second_sha,
            vec![json!({"app": ["Mover.app"]})],
        )],
        Arc::new(PanicRunner),
        reqwest::Client::new(),
    );
    install::run(&ctx_b, install_args(&["mover"], &appdir_b, true))
        .await
        .expect("force reinstall into changed appdir");

    assert!(
        !appdir_a.join("Mover.app").exists(),
        "old appdir target removed"
    );
    assert_eq!(
        std::fs::read(appdir_b.join("Mover.app/run")).expect("b run"),
        b"second"
    );
    let staging = fixture.env.caskroom.join(".staging");
    let leftovers = std::fs::read_dir(&staging).map(|e| e.count()).unwrap_or(0);
    assert_eq!(leftovers, 0, "staging empty after success");
}

#[tokio::test]
async fn force_cross_version_upgrade_backs_up_and_replaces_old_version() {
    let server = MockServer::start().await;
    let v1 = tar_gz(&[("Editor.app/run", b"one")]);
    let v1_sha = sha_hex(&v1);
    let v1_url = serve(&server, "/Editor1.tar.gz", v1).await;
    let v2 = tar_gz(&[("Editor.app/run", b"two")]);
    let v2_sha = sha_hex(&v2);
    let v2_url = serve(&server, "/Editor2.tar.gz", v2).await;

    let fixture = Fixture::new().macos();
    let appdir = fixture.env.home.join("Applications");

    let (ctx1, _r1) = fixture.context_casks(
        vec![cask(
            "editor",
            &v1_url,
            &v1_sha,
            vec![json!({"app": ["Editor.app"]})],
        )],
        Arc::new(PanicRunner),
        reqwest::Client::new(),
    );
    install::run(&ctx1, install_args(&["editor"], &appdir, false))
        .await
        .expect("v1 install");
    assert_eq!(
        std::fs::read(appdir.join("Editor.app/run")).expect("v1 run"),
        b"one"
    );

    // Force-install a DIFFERENT version: the whole-token machine must back up and
    // drop v1.0 (terminal defect #1), not skip it because the new dir differs.
    let mut value = cask(
        "editor",
        &v2_url,
        &v2_sha,
        vec![json!({"app": ["Editor.app"]})],
    );
    value["version"] = json!("2.0");
    let (ctx2, _r2) =
        fixture.context_casks(vec![value], Arc::new(PanicRunner), reqwest::Client::new());
    install::run(&ctx2, install_args(&["editor"], &appdir, true))
        .await
        .expect("v2 force install");

    assert_eq!(
        std::fs::read(appdir.join("Editor.app/run")).expect("v2 run"),
        b"two"
    );
    assert!(fixture.env.caskroom.join("editor/2.0").is_dir());
    assert!(
        !fixture.env.caskroom.join("editor/1.0").exists(),
        "old version removed"
    );
    assert!(!fixture.env.caskroom.join("editor/.metadata/1.0").exists());
    assert!(fixture.env.caskroom.join("editor/.metadata/2.0").is_dir());
    let staging = fixture.env.caskroom.join(".staging");
    assert_eq!(
        std::fs::read_dir(&staging).map(|e| e.count()).unwrap_or(0),
        0,
        "staging drained after success"
    );
}

#[tokio::test]
async fn force_cross_version_failure_restores_old_version_and_artifacts() {
    let server = MockServer::start().await;
    let v1 = tar_gz(&[("Editor.app/run", b"old-one")]);
    let v1_sha = sha_hex(&v1);
    let v1_url = serve(&server, "/CV1.tar.gz", v1).await;
    let v2 = tar_gz(&[
        ("Editor.app/run", b"new-two"),
        ("Second.app/run", b"new-second"),
    ]);
    let v2_sha = sha_hex(&v2);
    let v2_url = serve(&server, "/CV2.tar.gz", v2).await;

    let fixture = Fixture::new().macos();
    let appdir = fixture.env.home.join("Applications");

    let (ctx1, _r1) = fixture.context_casks(
        vec![cask(
            "editor",
            &v1_url,
            &v1_sha,
            vec![json!({"app": ["Editor.app"]})],
        )],
        Arc::new(PanicRunner),
        reqwest::Client::new(),
    );
    install::run(&ctx1, install_args(&["editor"], &appdir, false))
        .await
        .expect("v1 install");

    // An unrelated occupant blocks the v2 second-app target, so the v2 apply
    // fails after v1.0 is already backed up. Rollback must restore v1.0 exactly.
    std::fs::write(appdir.join("Second.app"), b"blocker").expect("blocker");

    let mut value = cask(
        "editor",
        &v2_url,
        &v2_sha,
        vec![
            json!({"app": ["Editor.app"]}),
            json!({"app": ["Second.app"]}),
        ],
    );
    value["version"] = json!("2.0");
    let (ctx2, _r2) =
        fixture.context_casks(vec![value], Arc::new(PanicRunner), reqwest::Client::new());
    let result = install::run(&ctx2, install_args(&["editor"], &appdir, true)).await;
    assert!(
        result.is_err(),
        "expected the v2 apply to refuse the collision"
    );

    // v1.0 dir, record, metadata, and deployed artifact are all restored; no 2.0.
    assert_eq!(
        std::fs::read(appdir.join("Editor.app/run")).expect("restored run"),
        b"old-one"
    );
    assert_eq!(
        std::fs::read(appdir.join("Second.app")).expect("blocker survives"),
        b"blocker"
    );
    assert!(fixture.env.caskroom.join("editor/1.0").is_dir());
    assert!(
        fixture
            .env
            .caskroom
            .join("editor/1.0/.zapbrew-record.json")
            .is_file()
    );
    assert!(
        !fixture.env.caskroom.join("editor/2.0").exists(),
        "no new version residue"
    );
    assert!(fixture.env.caskroom.join("editor/.metadata/1.0").is_dir());
    assert!(!fixture.env.caskroom.join("editor/.metadata/2.0").exists());
    let staging = fixture.env.caskroom.join(".staging");
    assert_eq!(
        std::fs::read_dir(&staging).map(|e| e.count()).unwrap_or(0),
        0,
        "staging drained after rollback"
    );
}

#[tokio::test]
async fn force_replace_backs_up_all_when_multiple_versions_present() {
    let server = MockServer::start().await;
    let body = tar_gz(&[("Three.app/run", b"three")]);
    let sha = sha_hex(&body);
    let url = serve(&server, "/Three.tar.gz", body).await;

    let fixture = Fixture::new().macos();
    let appdir = fixture.env.home.join("Applications");
    std::fs::create_dir_all(&appdir).expect("appdir");

    // Two pre-existing installed versions sharing one deployed target.
    for (version, unique) in [("1.0", "One.app"), ("2.0", "Two.app")] {
        seed_record(
            &fixture,
            "multi",
            version,
            appdir.as_str(),
            &[
                json!({"kind": "path", "target": appdir.join("Shared.app").to_string()}),
                json!({"kind": "path", "target": appdir.join(unique).to_string()}),
            ],
        );
        let meta = fixture
            .env
            .caskroom
            .join(format!("multi/.metadata/{version}/20260101/Casks"));
        std::fs::create_dir_all(&meta).expect("meta");
        std::fs::write(meta.join("multi.json"), b"{}\n").expect("receipt");
    }
    std::fs::create_dir_all(appdir.join("Shared.app")).expect("shared");
    std::fs::write(appdir.join("Shared.app/run"), b"shared").expect("shared run");
    std::fs::create_dir_all(appdir.join("One.app")).expect("one");
    std::fs::create_dir_all(appdir.join("Two.app")).expect("two");

    let mut value = cask("multi", &url, &sha, vec![json!({"app": ["Three.app"]})]);
    value["version"] = json!("3.0");
    let (ctx, _r) =
        fixture.context_casks(vec![value], Arc::new(PanicRunner), reqwest::Client::new());
    install::run(&ctx, install_args(&["multi"], &appdir, true))
        .await
        .expect("force replace-all");

    // Both old versions and every old deployed target (shared deduped) are gone.
    assert!(!appdir.join("Shared.app").exists());
    assert!(!appdir.join("One.app").exists());
    assert!(!appdir.join("Two.app").exists());
    assert!(appdir.join("Three.app/run").is_file());
    assert!(!fixture.env.caskroom.join("multi/1.0").exists());
    assert!(!fixture.env.caskroom.join("multi/2.0").exists());
    assert!(fixture.env.caskroom.join("multi/3.0").is_dir());
    assert!(!fixture.env.caskroom.join("multi/.metadata/1.0").exists());
    assert!(!fixture.env.caskroom.join("multi/.metadata/2.0").exists());
    assert!(fixture.env.caskroom.join("multi/.metadata/3.0").is_dir());
    let staging = fixture.env.caskroom.join(".staging");
    assert_eq!(
        std::fs::read_dir(&staging).map(|e| e.count()).unwrap_or(0),
        0,
        "staging drained after replace-all"
    );
}

fn _use_ctx(_ctx: &Ctx) {}

#[tokio::test]
async fn linux_install_refuses_malformed_and_duplicate_targets() {
    let fixture = Fixture::new();
    let appdir = fixture.env.home.join("Applications");
    for (name, artifact) in [
        (
            "non-string-target",
            json!({"binary": ["tool"], "target": 123}),
        ),
        ("empty-target", json!({"binary": ["tool"], "target": ""})),
        (
            "duplicate-target",
            json!({"binary": ["tool", {"target": "nested"}], "target": "top"}),
        ),
        (
            "nested-non-string-target",
            json!({"binary": ["tool", {"target": 123}]}),
        ),
        (
            "nested-empty-target",
            json!({"binary": ["tool", {"target": ""}]}),
        ),
    ] {
        let value = cask(
            name,
            "https://example.test/App.tar.gz",
            "no_check",
            vec![artifact],
        );
        let (ctx, _reporter) =
            fixture.context_casks(vec![value], Arc::new(PanicRunner), reqwest::Client::new());
        let result = install::run(&ctx, install_args(&[name], &appdir, false)).await;
        assert!(result.is_err(), "expected refusal for {name}");
        assert!(
            !fixture.env.caskroom.join(".staging").exists(),
            "no staging for {name}"
        );
    }
    assert!(
        !fixture.env.locks.exists(),
        "locks dir stays absent for target refusals"
    );
}

#[tokio::test]
async fn linux_install_refuses_pkg_ignored_modifiers() {
    let fixture = Fixture::new();
    let appdir = fixture.env.home.join("Applications");
    for (name, artifact) in [
        (
            "pkg-allow-untrusted",
            json!({"pkg": "Foo.pkg", "allow_untrusted": true}),
        ),
        (
            "pkg-choices",
            json!({"pkg": "Foo.pkg", "choices": [{"key": "foo"}]}),
        ),
        (
            "pkg-nested-untrusted",
            json!({"pkg": ["Foo.pkg", {"allow_untrusted": true}]}),
        ),
    ] {
        let value = cask(
            name,
            "https://example.test/App.tar.gz",
            "no_check",
            vec![artifact],
        );
        let (ctx, _reporter) =
            fixture.context_casks(vec![value], Arc::new(PanicRunner), reqwest::Client::new());
        let result = install::run(&ctx, install_args(&[name], &appdir, false)).await;
        assert!(result.is_err(), "expected refusal for {name}");
        assert!(
            !fixture.env.caskroom.join(".staging").exists(),
            "no staging for {name}"
        );
    }
}

#[tokio::test]
async fn linux_install_refuses_malformed_uninstall_zap() {
    let fixture = Fixture::new();
    let appdir = fixture.env.home.join("Applications");
    for (name, artifact) in [
        ("uninstall-string", json!({"uninstall": "x"})),
        ("uninstall-empty-object", json!({"uninstall": [{}]})),
        (
            "uninstall-unknown",
            json!({"uninstall": [{"reboot": true}]}),
        ),
        (
            "uninstall-launchctl-empty",
            json!({"uninstall": [{"launchctl": ""}]}),
        ),
        (
            "uninstall-pkgutil-empty",
            json!({"uninstall": [{"pkgutil": []}]}),
        ),
        (
            "uninstall-signal-string",
            json!({"uninstall": [{"signal": "TERM"}]}),
        ),
        (
            "uninstall-signal-empty-key",
            json!({"uninstall": [{"signal": {"": "app"}}]}),
        ),
        (
            "uninstall-signal-empty-value",
            json!({"uninstall": [{"signal": {"TERM": ""}}]}),
        ),
        (
            "uninstall-signal-empty-pair",
            json!({"uninstall": [{"signal": [[]]}]}),
        ),
        (
            "uninstall-signal-bad-pair",
            json!({"uninstall": [{"signal": [["TERM", "app", "extra"]]}]}),
        ),
        (
            "uninstall-delete-empty",
            json!({"uninstall": [{"delete": ["ok", ""]}]}),
        ),
        ("zap-delete-empty", json!({"zap": [{"delete": ["ok", ""]}]})),
    ] {
        let value = cask(
            name,
            "https://example.test/App.tar.gz",
            "no_check",
            vec![artifact],
        );
        let (ctx, _reporter) =
            fixture.context_casks(vec![value], Arc::new(PanicRunner), reqwest::Client::new());
        let result = install::run(&ctx, install_args(&[name], &appdir, false)).await;
        assert!(result.is_err(), "expected refusal for {name}");
        assert!(
            !fixture.env.caskroom.join(".staging").exists(),
            "no staging for {name}"
        );
    }
    assert!(
        !fixture.env.locks.exists(),
        "locks dir stays absent for directive refusals"
    );
}

#[tokio::test]
async fn linux_install_refuses_unsafe_version_and_token() {
    let fixture = Fixture::new();
    let appdir = fixture.env.home.join("Applications");
    for version in ["..", "a/b", "/etc"] {
        let mut value = cask(
            "badver",
            "https://example.test/App.tar.gz",
            "no_check",
            vec![json!({"binary": ["tool"]})],
        );
        value["version"] = json!(version);
        let (ctx, _reporter) =
            fixture.context_casks(vec![value], Arc::new(PanicRunner), reqwest::Client::new());
        let result = install::run(&ctx, install_args(&["badver"], &appdir, false)).await;
        assert!(result.is_err(), "expected refusal for version {version}");
        assert!(
            !fixture.env.locks.exists(),
            "no lock dir for unsafe version {version}"
        );
    }
    for token in ["..", "a/b", "../x"] {
        let value = cask(
            token,
            "https://example.test/App.tar.gz",
            "no_check",
            vec![json!({"binary": ["tool"]})],
        );
        let (ctx, _reporter) =
            fixture.context_casks(vec![value], Arc::new(PanicRunner), reqwest::Client::new());
        let result = install::run(&ctx, install_args(&[token], &appdir, false)).await;
        assert!(result.is_err(), "expected refusal for token {token}");
        assert!(
            !fixture.env.locks.exists(),
            "no lock dir for unsafe token {token}"
        );
    }
}

#[tokio::test]
async fn linux_install_refuses_dmg_url_forms_before_download() {
    let server = MockServer::start().await;
    let body = tar_gz(&[("bin/tool", b"tool")]);
    let sha = sha_hex(&body);
    let mut urls: Vec<(&'static str, String)> = Vec::new();
    for route in ["/App.dmg", "/App.DMG", "/App%2Edmg"] {
        let url = serve(&server, route, body.clone()).await;
        urls.push((route, url));
    }
    Mock::given(method("GET"))
        .and(path("/App.dmg"))
        .and(wiremock::matchers::query_param("x", "y"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(body.clone()))
        .mount(&server)
        .await;
    urls.push(("query", format!("{}{}?x=y", server.uri(), "/App.dmg")));
    Mock::given(method("GET"))
        .and(path("/App.dmg"))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(body.clone()))
        .mount(&server)
        .await;
    urls.push(("fragment", format!("{}{}#frag", server.uri(), "/App.dmg")));
    for (route, url) in urls {
        let fixture = Fixture::new();
        let appdir = fixture.env.home.join("Applications");
        let value = cask("disky", &url, &sha, vec![json!({"binary": ["tool"]})]);
        let (ctx, _reporter) =
            fixture.context_casks(vec![value], Arc::new(PanicRunner), reqwest::Client::new());
        let result = install::run(&ctx, install_args(&["disky"], &appdir, false)).await;
        let error = err(result);
        assert!(
            matches!(&error, OpError::Refusal { message } if message == "Cask 'disky' ships a macOS disk image, which is unavailable on Linux."),
            "{route}: expected .dmg refusal, got {error:?}"
        );
        assert!(
            !fixture.env.caskroom.join(".staging").exists(),
            "{route}: no staging"
        );
        assert!(!fixture.env.locks.exists(), "{route}: no locks");
    }
    assert!(
        server
            .received_requests()
            .await
            .is_none_or(|requests| requests.is_empty()),
        "no download for any .dmg form"
    );
}

#[tokio::test]
async fn linux_install_early_refusal_leaves_locks_dir_absent() {
    let fixture = Fixture::new();
    let appdir = fixture.env.home.join("Applications");
    fs::remove_dir_all(&fixture.env.locks).ok();
    assert!(!fixture.env.locks.exists());
    let value = cask(
        "bad",
        "https://example.test/App.tar.gz",
        "no_check",
        vec![json!({"binary": ["tool", {"target": 123}]})],
    );
    let (ctx, _reporter) =
        fixture.context_casks(vec![value], Arc::new(PanicRunner), reqwest::Client::new());
    let result = install::run(&ctx, install_args(&["bad"], &appdir, false)).await;
    assert!(result.is_err());
    assert!(!fixture.env.locks.exists(), "locks dir must stay absent");
    assert!(!fixture.env.caskroom.join(".staging").exists());
}

#[tokio::test]
async fn linux_install_refuses_symlinked_appdir_ancestor() {
    let fixture = Fixture::new();
    let appdir = fixture.env.home.join("Applications");
    let real = fixture.env.home.join("real-apps");
    fs::create_dir_all(&real).expect("real appdir");
    std::os::unix::fs::symlink(real.as_std_path(), appdir.as_std_path()).expect("appdir symlink");
    let value = cask(
        "symapp",
        "https://example.test/App.tar.gz",
        "no_check",
        vec![json!({"binary": ["tool"]})],
    );
    let (ctx, _reporter) =
        fixture.context_casks(vec![value], Arc::new(PanicRunner), reqwest::Client::new());
    let result = install::run(&ctx, install_args(&["symapp"], &appdir, false)).await;
    let error = err(result);
    assert!(
        error.to_string().contains("symlink"),
        "expected symlink refusal, got {error}"
    );
    assert!(!fixture.env.caskroom.join(".staging").exists());
}

#[tokio::test]
async fn linux_install_refuses_symlinked_caskroom_token_ancestor() {
    let server = MockServer::start().await;
    let body = tar_gz(&[("bin/tool", b"tool")]);
    let sha = sha_hex(&body);
    let url = serve(&server, "/App.tar.gz", body).await;
    let fixture = Fixture::new();
    let appdir = fixture.env.home.join("Applications");
    fs::create_dir_all(&fixture.env.caskroom).expect("Caskroom");
    let real = fixture.env.caskroom.join("real-token");
    fs::create_dir_all(&real).expect("real token");
    std::os::unix::fs::symlink(
        real.as_std_path(),
        fixture.env.caskroom.join("symtoken").as_std_path(),
    )
    .expect("token symlink");
    let value = cask("symtoken", &url, &sha, vec![json!({"binary": ["tool"]})]);
    let (ctx, _reporter) =
        fixture.context_casks(vec![value], Arc::new(PanicRunner), reqwest::Client::new());
    let result = install::run(&ctx, install_args(&["symtoken"], &appdir, false)).await;
    let error = err(result);
    assert!(
        error.to_string().contains("symlink") || error.to_string().contains("Caskroom"),
        "expected Caskroom refusal, got {error}"
    );
    assert!(!fixture.env.caskroom.join("symtoken/1.0").exists());
    assert!(!fixture.env.caskroom.join(".staging").exists());
}

#[tokio::test]
async fn linux_install_refuses_symlinked_caskroom_metadata_ancestor() {
    let server = MockServer::start().await;
    let body = tar_gz(&[("bin/tool", b"tool")]);
    let sha = sha_hex(&body);
    let url = serve(&server, "/App.tar.gz", body).await;
    let fixture = Fixture::new();
    let appdir = fixture.env.home.join("Applications");
    fs::create_dir_all(fixture.env.caskroom.join("metatoken")).expect("metadata token");
    let real = fixture.env.caskroom.join("real-meta");
    fs::create_dir_all(&real).expect("real metadata");
    std::os::unix::fs::symlink(
        real.as_std_path(),
        fixture
            .env
            .caskroom
            .join("metatoken/.metadata")
            .as_std_path(),
    )
    .expect("metadata symlink");
    let value = cask("metatoken", &url, &sha, vec![json!({"binary": ["tool"]})]);
    let (ctx, _reporter) =
        fixture.context_casks(vec![value], Arc::new(PanicRunner), reqwest::Client::new());
    let result = install::run(&ctx, install_args(&["metatoken"], &appdir, false)).await;
    let error = err(result);
    assert!(
        error.to_string().contains("symlink") || error.to_string().contains("Caskroom"),
        "expected Caskroom refusal, got {error}"
    );
    assert!(!fixture.env.caskroom.join("metatoken/1.0").exists());
    assert!(
        !fixture
            .env
            .caskroom
            .join("metatoken/.metadata/1.0")
            .exists()
    );
}
