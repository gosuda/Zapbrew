mod support;

use std::fs;
use std::sync::{Arc, Mutex};

use camino::Utf8Path;
use serde_json::{Value, json};
use support::Fixture;
use zapbrew_ops::OpError;
use zapbrew_ops::cask::uninstall::{self, Args};
use zapbrew_prefix::{CommandOutput, CommandRunner, CommandSpec};

#[derive(Default)]
struct RecordingRunner(Mutex<Vec<Vec<String>>>);

impl RecordingRunner {
    fn calls(&self) -> Vec<Vec<String>> {
        self.0.lock().expect("calls").clone()
    }
}

impl CommandRunner for RecordingRunner {
    fn run(&self, spec: &CommandSpec) -> Result<CommandOutput, std::io::Error> {
        let mut call = vec![spec.program().to_string_lossy().into_owned()];
        call.extend(
            spec.arguments()
                .iter()
                .map(|arg| arg.to_string_lossy().into_owned()),
        );
        self.0.lock().expect("calls").push(call);
        use std::os::unix::process::ExitStatusExt;
        Ok(CommandOutput::new(
            std::process::ExitStatus::from_raw(0),
            Vec::new(),
            Vec::new(),
        ))
    }
}

fn receipt(fixture: &Fixture, token: &str, raw: &Value) {
    let version = fixture.env.caskroom.join(token).join("1.0");
    fs::create_dir_all(&version).expect("version");
    let path = fixture
        .env
        .caskroom
        .join(token)
        .join(".metadata/1.0/20260804010203/Casks")
        .join(format!("{token}.json"));
    fs::create_dir_all(path.parent().expect("parent")).expect("receipt parent");
    fs::write(path, serde_json::to_vec_pretty(raw).expect("raw")).expect("receipt");
}

fn cask(fixture: &Fixture) -> Value {
    json!({
        "token": "remove-me",
        "version": "1.0",
        "sha256": "no_check",
        "url": "https://example.test/app.zip",
        "artifacts": [
            {"artifact": ["stored.txt"], "target": format!("{}/installed.txt", fixture.env.home)},
            {"uninstall": [
                {"launchctl": "com.example.remove"},
                {"pkgutil": "com.example.pkg"},
                {"delete": format!("{}/normal-state", fixture.env.home)},
                {"quit": "com.example.app"},
                {"signal": {"TERM": "com.example.app"}}
            ]},
            {"zap": [
                {"trash": format!("{}/zap-state", fixture.env.home)},
                {"rmdir": format!("{}/empty-dir", fixture.env.home)}
            ]}
        ]
    })
}

fn setup(fixture: &Fixture, raw: &Value) {
    fs::create_dir_all(&fixture.env.home).expect("home");
    receipt(fixture, "remove-me", raw);
    fs::write(fixture.env.home.join("installed.txt"), b"installed").expect("installed target");
    fs::write(fixture.env.home.join("normal-state"), b"normal").expect("normal state");
    fs::write(fixture.env.home.join("zap-state"), b"zap").expect("zap state");
    fs::create_dir_all(fixture.env.home.join("empty-dir")).expect("empty");
    let plist = fixture
        .env
        .home
        .join("Library/LaunchAgents/com.example.remove.plist");
    fs::create_dir_all(plist.parent().expect("parent")).expect("launch agents");
    fs::write(plist, b"plist").expect("plist");
}

#[tokio::test]
async fn uninstall_reverses_artifacts_and_only_uninstall_directives() {
    let fixture = Fixture::new().macos();
    let raw = cask(&fixture);
    setup(&fixture, &raw);
    let runner = Arc::new(RecordingRunner::default());
    let (ctx, reporter) = fixture.context_casks(vec![], runner.clone(), reqwest::Client::new());

    uninstall::run(
        &ctx,
        Args {
            tokens: vec!["remove-me".to_owned()],
            zap: false,
        },
    )
    .await
    .expect("uninstall");

    assert!(!fixture.env.home.join("installed.txt").exists());
    assert!(!fixture.env.home.join("normal-state").exists());
    assert!(fixture.env.home.join("zap-state").exists());
    assert!(fixture.env.home.join("empty-dir").exists());
    assert!(!fixture.env.caskroom.join("remove-me").exists());
    let plist = fixture
        .env
        .home
        .join("Library/LaunchAgents/com.example.remove.plist");
    assert_eq!(
        runner.calls(),
        vec![
            vec![
                "/bin/launchctl".to_owned(),
                "unload".to_owned(),
                plist.to_string(),
            ],
            vec![
                "/usr/sbin/pkgutil".to_owned(),
                "--forget".to_owned(),
                "com.example.pkg".to_owned(),
            ],
        ]
    );
    assert_eq!(
        reporter.take(),
        vec![
            "opoo:Skipping unsupported cask quit directive.",
            "opoo:Skipping unsupported cask signal directive.",
        ]
    );
}

#[tokio::test]
async fn zap_adds_trash_and_rmdir_directives() {
    let fixture = Fixture::new().macos();
    let raw = cask(&fixture);
    setup(&fixture, &raw);
    let runner = Arc::new(RecordingRunner::default());
    let (ctx, _reporter) = fixture.context_casks(vec![], runner, reqwest::Client::new());

    uninstall::run(
        &ctx,
        Args {
            tokens: vec!["remove-me".to_owned()],
            zap: true,
        },
    )
    .await
    .expect("zap");
    assert!(!fixture.env.home.join("zap-state").exists());
    assert!(!fixture.env.home.join("empty-dir").exists());
}

#[tokio::test]
async fn missing_install_and_missing_receipt_refuse() {
    let fixture = Fixture::new().macos();
    let runner = Arc::new(RecordingRunner::default());
    let (ctx, _reporter) = fixture.context_casks(vec![], runner, reqwest::Client::new());
    let missing = uninstall::run(
        &ctx,
        Args {
            tokens: vec!["ghost".to_owned()],
            zap: false,
        },
    )
    .await;
    assert!(
        matches!(missing, Err(OpError::Refusal { message }) if message == "Cask 'ghost' is unavailable.")
    );

    fs::create_dir_all(fixture.env.caskroom.join("broken/1.0")).expect("version");
    let broken = uninstall::run(
        &ctx,
        Args {
            tokens: vec!["broken".to_owned()],
            zap: false,
        },
    )
    .await;
    assert!(
        matches!(broken, Err(OpError::InvalidState { reason }) if reason == "Cask 'broken' has no stored receipt.")
    );
}

#[tokio::test]
async fn uninstall_resolves_old_token_alias() {
    let fixture = Fixture::new().macos();
    let raw = json!({
        "token": "everything",
        "old_tokens": ["every-thing"],
        "version": "1.0",
        "sha256": "no_check",
        "url": "https://example.test/app.zip",
        "artifacts": [
            {"artifact": ["stored.txt"], "target": format!("{}/installed.txt", fixture.env.home)}
        ]
    });
    fs::create_dir_all(&fixture.env.home).expect("home");
    receipt(&fixture, "everything", &raw);
    fs::write(fixture.env.home.join("installed.txt"), b"installed").expect("target");

    let runner = Arc::new(RecordingRunner::default());
    let (ctx, _reporter) = fixture.context_casks(vec![raw.clone()], runner, reqwest::Client::new());

    uninstall::run(
        &ctx,
        Args {
            tokens: vec!["every-thing".to_owned()],
            zap: false,
        },
    )
    .await
    .expect("old-token uninstall");

    assert!(!fixture.env.home.join("installed.txt").exists());
    assert!(!fixture.env.caskroom.join("everything").exists());
}

#[tokio::test]
async fn uninstall_removed_from_catalog_uses_receipt() {
    let fixture = Fixture::new().macos();
    let raw = json!({
        "token": "vanished",
        "version": "1.0",
        "sha256": "no_check",
        "url": "https://example.test/app.zip",
        "artifacts": [
            {"artifact": ["stored.txt"], "target": format!("{}/installed.txt", fixture.env.home)}
        ]
    });
    fs::create_dir_all(&fixture.env.home).expect("home");
    receipt(&fixture, "vanished", &raw);
    fs::write(fixture.env.home.join("installed.txt"), b"installed").expect("target");

    // Live catalog no longer lists the cask; uninstall relies on the receipt tree.
    let runner = Arc::new(RecordingRunner::default());
    let (ctx, _reporter) = fixture.context_casks(vec![], runner, reqwest::Client::new());

    uninstall::run(
        &ctx,
        Args {
            tokens: vec!["vanished".to_owned()],
            zap: false,
        },
    )
    .await
    .expect("uninstall from stored receipt");

    assert!(!fixture.env.home.join("installed.txt").exists());
    assert!(!fixture.env.caskroom.join("vanished").exists());
}

#[tokio::test]
async fn unsafe_remove_path_fails_before_any_directive() {
    let fixture = Fixture::new().macos();
    let raw = json!({
        "token": "unsafe",
        "version": "1.0",
        "sha256": "no_check",
        "url": "https://example.test/app.zip",
        "artifacts": [{"uninstall": [{"delete": "/etc"}, {"pkgutil": "must.not.run"}]}]
    });
    receipt(&fixture, "unsafe", &raw);
    let runner = Arc::new(RecordingRunner::default());
    let (ctx, _reporter) = fixture.context_casks(vec![], runner.clone(), reqwest::Client::new());
    let result = uninstall::run(
        &ctx,
        Args {
            tokens: vec!["unsafe".to_owned()],
            zap: false,
        },
    )
    .await;
    assert!(result.is_err());
    assert!(runner.calls().is_empty());
    assert!(fixture.env.caskroom.join("unsafe/1.0").is_dir());
}

fn _path(_path: &Utf8Path) {}
