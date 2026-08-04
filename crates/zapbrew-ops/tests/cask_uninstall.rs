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

/// Seed one installed version tree: raw receipt under `.metadata/<version>` plus
/// the typed install record a real install promotes into the version tree.
/// Deployed targets and directives are derived from the fixture cask payload,
/// mirroring `InstallRecord::from_plan`.
fn seed_version(fixture: &Fixture, token: &str, version: &str, raw: &Value, appdir: &str) {
    let version_dir = fixture.env.caskroom.join(token).join(version);
    fs::create_dir_all(&version_dir).expect("version");
    let receipt_path = fixture
        .env
        .caskroom
        .join(token)
        .join(format!(".metadata/{version}/20260804010203/Casks"))
        .join(format!("{token}.json"));
    fs::create_dir_all(receipt_path.parent().expect("parent")).expect("receipt parent");
    fs::write(receipt_path, serde_json::to_vec_pretty(raw).expect("raw")).expect("receipt");

    let mut artifacts = Vec::new();
    let mut uninstall = Vec::new();
    let mut zap = Vec::new();
    for artifact in raw["artifacts"].as_array().expect("artifacts") {
        let object = artifact.as_object().expect("artifact object");
        for (kind, value) in object {
            match kind.as_str() {
                // `target` is artifact metadata read alongside the kind key.
                "target" => {}
                "uninstall" => uninstall.push(value.clone()),
                "zap" => zap.push(value.clone()),
                "app" | "suite" => {
                    let (source, target) = source_and_target(value, artifact);
                    artifacts.push(json!({
                        "kind": "path",
                        "target": target.unwrap_or_else(|| format!("{appdir}/{}", leaf(&source))),
                    }));
                }
                "binary" | "manpage" => {
                    let (source, target) = source_and_target(value, artifact);
                    let name = target
                        .as_deref()
                        .map_or_else(|| leaf(&source).to_owned(), |value| leaf(value).to_owned());
                    let dir = if kind == "manpage" {
                        let section = source
                            .trim_end_matches(".gz")
                            .rsplit('.')
                            .next()
                            .expect("man section");
                        format!("{}/share/man/man{section}", fixture.env.prefix)
                    } else {
                        format!("{}/bin", fixture.env.prefix)
                    };
                    artifacts.push(json!({"kind": "symlink", "target": format!("{dir}/{name}")}));
                }
                "pkg" => {
                    let (source, _) = source_and_target(value, artifact);
                    artifacts.push(json!({"kind": "pkg", "source": source}));
                }
                "artifact" => {
                    let (source, target) = source_and_target(value, artifact);
                    artifacts.push(json!({
                        "kind": "path",
                        "target": target.unwrap_or_else(|| format!("{appdir}/{}", leaf(&source))),
                    }));
                }
                _ => {
                    // Copy-family kinds (font, prefpane, completions, ...) land in
                    // Library or prefix dirs; none of the uninstall fixtures use
                    // them, so seeding panics loudly if one appears.
                    panic!("fixture kind '{kind}' has no seeded target mapping");
                }
            }
        }
    }
    let record = json!({
        "schema": 1,
        "token": token,
        "version": version,
        "appdir": appdir,
        "artifacts": artifacts,
        "uninstall": uninstall,
        "zap": zap,
    });
    let mut bytes = serde_json::to_vec_pretty(&record).expect("record json");
    bytes.push(b'\n');
    fs::write(version_dir.join(".zapbrew-record.json"), bytes).expect("record");
}

/// Seed a 1.0 install, the shape every pre-existing uninstall fixture uses.
fn receipt(fixture: &Fixture, token: &str, raw: &Value) {
    seed_version(fixture, token, "1.0", raw, "/Applications");
}

fn source_and_target(value: &Value, artifact: &Value) -> (String, Option<String>) {
    let source = match value {
        Value::String(source) => source.clone(),
        Value::Array(values) => values[0].as_str().expect("source").to_owned(),
        _ => panic!("artifact source shape"),
    };
    let target = artifact
        .get("target")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| {
            value
                .as_array()
                .and_then(|values| values.get(1))
                .and_then(Value::as_object)
                .and_then(|object| object.get("target"))
                .and_then(Value::as_str)
                .map(str::to_owned)
        });
    (source, target)
}

fn leaf(path: &str) -> &str {
    path.trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or(path)
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

    // A version dir with no typed record aborts before any mutation.
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
        matches!(broken, Err(OpError::InvalidState { ref reason }) if reason.contains("record") && reason.contains("is missing")),
        "expected missing-record refusal, got {broken:?}"
    );
    assert!(fixture.env.caskroom.join("broken/1.0").is_dir());
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

#[tokio::test]
async fn traversal_launchctl_label_refuses_before_any_command() {
    let fixture = Fixture::new().macos();
    let raw = json!({
        "token": "evil-label",
        "version": "1.0",
        "sha256": "no_check",
        "url": "https://example.test/app.zip",
        "artifacts": [{"uninstall": [{"launchctl": "../evil"}]}]
    });
    receipt(&fixture, "evil-label", &raw);
    // A plist a naive traversal would unload and remove; it must survive.
    let sentinel = fixture.env.home.join("Library/evil.plist");
    fs::create_dir_all(sentinel.parent().expect("parent")).expect("library");
    fs::write(&sentinel, b"external-sentinel").expect("sentinel");

    let runner = Arc::new(RecordingRunner::default());
    let (ctx, _reporter) = fixture.context_casks(vec![], runner.clone(), reqwest::Client::new());
    let result = uninstall::run(
        &ctx,
        Args {
            tokens: vec!["evil-label".to_owned()],
            zap: false,
        },
    )
    .await;

    assert!(
        matches!(&result, Err(OpError::InvalidState { reason }) if reason.contains("launchctl label")),
        "expected invalid-label refusal, got {result:?}"
    );
    assert!(runner.calls().is_empty(), "no host command may run");
    assert_eq!(
        fs::read(&sentinel).expect("sentinel survives"),
        b"external-sentinel"
    );
    assert!(fixture.env.caskroom.join("evil-label/1.0").is_dir());
}

#[tokio::test]
async fn traversal_and_nested_and_absolute_tokens_refuse_before_join() {
    let fixture = Fixture::new().macos();
    // A sentinel a naive `caskroom.join("../evil")` would reach; it must survive.
    let escape = fixture
        .env
        .caskroom
        .parent()
        .expect("caskroom parent")
        .join("evil");
    fs::create_dir_all(&escape).expect("escape sentinel");
    let runner = Arc::new(RecordingRunner::default());
    let (ctx, _reporter) = fixture.context_casks(vec![], runner.clone(), reqwest::Client::new());

    for token in ["../evil", "nested/token", "/etc", ".", "..", ""] {
        let result = uninstall::run(
            &ctx,
            Args {
                tokens: vec![token.to_owned()],
                zap: false,
            },
        )
        .await;
        assert!(
            matches!(&result, Err(OpError::Refusal { message }) if message == &format!("Cask '{token}' is unavailable.")),
            "token {token:?} must refuse as unavailable, got {result:?}"
        );
    }
    assert!(runner.calls().is_empty(), "no host command may run");
    assert!(escape.is_dir(), "traversal sentinel must be untouched");
}

#[tokio::test]
async fn symlink_token_refuses_and_preserves_target() {
    let fixture = Fixture::new().macos();
    let raw = json!({
        "token": "real",
        "version": "1.0",
        "sha256": "no_check",
        "url": "https://example.test/app.zip",
        "artifacts": [{"artifact": ["stored.txt"], "target": format!("{}/installed.txt", fixture.env.home)}]
    });
    fs::create_dir_all(&fixture.env.home).expect("home");
    receipt(&fixture, "real", &raw);
    // A Caskroom entry that is itself a symlink to the real install must not be a
    // valid uninstall target: no-follow metadata rejects it.
    let link = fixture.env.caskroom.join("link");
    std::os::unix::fs::symlink(fixture.env.caskroom.join("real"), &link).expect("symlink");
    let runner = Arc::new(RecordingRunner::default());
    let (ctx, _reporter) = fixture.context_casks(vec![], runner.clone(), reqwest::Client::new());

    let result = uninstall::run(
        &ctx,
        Args {
            tokens: vec!["link".to_owned()],
            zap: false,
        },
    )
    .await;
    assert!(
        matches!(&result, Err(OpError::Refusal { message }) if message == "Cask 'link' is unavailable."),
        "symlink token must refuse, got {result:?}"
    );
    assert!(runner.calls().is_empty());
    assert!(
        fixture.env.caskroom.join("real/1.0").is_dir(),
        "real install survives"
    );
}

#[tokio::test]
async fn uninstall_removes_all_installed_versions_in_one_run() {
    let fixture = Fixture::new().macos();
    fs::create_dir_all(&fixture.env.home).expect("home");
    let raw1 = json!({
        "token": "multi", "version": "1.0", "sha256": "no_check",
        "url": "https://example.test/a.zip",
        "artifacts": [
            {"artifact": ["s.txt"], "target": format!("{}/shared.txt", fixture.env.home)},
            {"artifact": ["o.txt"], "target": format!("{}/one.txt", fixture.env.home)}
        ]
    });
    let raw2 = json!({
        "token": "multi", "version": "2.0", "sha256": "no_check",
        "url": "https://example.test/b.zip",
        "artifacts": [
            {"artifact": ["s.txt"], "target": format!("{}/shared.txt", fixture.env.home)},
            {"artifact": ["t.txt"], "target": format!("{}/two.txt", fixture.env.home)}
        ]
    });
    seed_version(&fixture, "multi", "1.0", &raw1, fixture.env.home.as_str());
    seed_version(&fixture, "multi", "2.0", &raw2, fixture.env.home.as_str());
    fs::write(fixture.env.home.join("shared.txt"), b"s").expect("shared");
    fs::write(fixture.env.home.join("one.txt"), b"1").expect("one");
    fs::write(fixture.env.home.join("two.txt"), b"2").expect("two");

    let runner = Arc::new(RecordingRunner::default());
    let (ctx, _r) = fixture.context_casks(vec![], runner, reqwest::Client::new());
    uninstall::run(
        &ctx,
        Args {
            tokens: vec!["multi".to_owned()],
            zap: false,
        },
    )
    .await
    .expect("uninstall every installed version in one run");

    // Both versions' targets (shared deduped) removed; token dir pruned.
    assert!(!fixture.env.home.join("shared.txt").exists());
    assert!(!fixture.env.home.join("one.txt").exists());
    assert!(!fixture.env.home.join("two.txt").exists());
    assert!(
        !fixture.env.caskroom.join("multi").exists(),
        "token dir pruned after all versions removed"
    );
}

#[tokio::test]
async fn multi_version_preflight_failure_removes_nothing() {
    let fixture = Fixture::new().macos();
    fs::create_dir_all(&fixture.env.home).expect("home");
    // The older version carries an out-of-root delete directive; preflight of the
    // whole set must abort before any target or directive runs.
    let raw1 = json!({
        "token": "guard", "version": "1.0", "sha256": "no_check",
        "url": "https://example.test/a.zip",
        "artifacts": [
            {"artifact": ["o.txt"], "target": format!("{}/one.txt", fixture.env.home)},
            {"uninstall": [{"delete": "/etc"}]}
        ]
    });
    let raw2 = json!({
        "token": "guard", "version": "2.0", "sha256": "no_check",
        "url": "https://example.test/b.zip",
        "artifacts": [
            {"artifact": ["t.txt"], "target": format!("{}/two.txt", fixture.env.home)}
        ]
    });
    seed_version(&fixture, "guard", "1.0", &raw1, fixture.env.home.as_str());
    seed_version(&fixture, "guard", "2.0", &raw2, fixture.env.home.as_str());
    fs::write(fixture.env.home.join("one.txt"), b"1").expect("one");
    fs::write(fixture.env.home.join("two.txt"), b"2").expect("two");

    let runner = Arc::new(RecordingRunner::default());
    let (ctx, _r) = fixture.context_casks(vec![], runner.clone(), reqwest::Client::new());
    let result = uninstall::run(
        &ctx,
        Args {
            tokens: vec!["guard".to_owned()],
            zap: false,
        },
    )
    .await;
    assert!(result.is_err(), "unsafe directive must abort the whole run");
    assert!(runner.calls().is_empty(), "no host command may run");
    assert!(fixture.env.home.join("one.txt").exists());
    assert!(fixture.env.home.join("two.txt").exists());
    assert!(fixture.env.caskroom.join("guard/1.0").is_dir());
    assert!(fixture.env.caskroom.join("guard/2.0").is_dir());
}

#[tokio::test]
async fn tampered_record_appdir_cannot_widen_removal_roots() {
    let fixture = Fixture::new().macos();
    fs::create_dir_all(&fixture.env.home).expect("home");
    let raw = json!({
        "token": "evil", "version": "1.0", "sha256": "no_check",
        "url": "https://example.test/a.zip",
        "artifacts": [
            {"artifact": ["v.txt"], "target": format!("{}/victim.txt", fixture.env.home)}
        ]
    });
    // A tampered record points appdir at `/` so every absolute target would
    // lexically "fit" inside the appdir root; validation must reject it before
    // any removal so the victim survives.
    seed_version(&fixture, "evil", "1.0", &raw, "/");
    fs::write(fixture.env.home.join("victim.txt"), b"v").expect("victim");

    let runner = Arc::new(RecordingRunner::default());
    let (ctx, _r) = fixture.context_casks(vec![], runner.clone(), reqwest::Client::new());
    let result = uninstall::run(
        &ctx,
        Args {
            tokens: vec!["evil".to_owned()],
            zap: false,
        },
    )
    .await;
    assert!(result.is_err(), "broad appdir record must refuse");
    assert!(runner.calls().is_empty(), "no host command may run");
    assert!(
        fixture.env.home.join("victim.txt").exists(),
        "victim survives"
    );
    assert!(fixture.env.caskroom.join("evil/1.0").is_dir());
}

#[tokio::test]
async fn symlinked_record_file_refuses_before_read() {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new().macos();
    fs::create_dir_all(&fixture.env.home).expect("home");
    let raw = json!({
        "token": "linky", "version": "1.0", "sha256": "no_check",
        "url": "https://example.test/a.zip",
        "artifacts": [{"artifact": ["o.txt"], "target": format!("{}/one.txt", fixture.env.home)}]
    });
    seed_version(&fixture, "linky", "1.0", &raw, fixture.env.home.as_str());
    fs::write(fixture.env.home.join("one.txt"), b"1").expect("one");
    // Replace the record file with a symlink to an external payload; the no-follow
    // check must reject it without dereferencing the link target.
    let record = fixture.env.caskroom.join("linky/1.0/.zapbrew-record.json");
    fs::remove_file(&record).expect("remove record");
    symlink(fixture.env.home.join("one.txt"), &record).expect("record symlink");

    let runner = Arc::new(RecordingRunner::default());
    let (ctx, _r) = fixture.context_casks(vec![], runner.clone(), reqwest::Client::new());
    let result = uninstall::run(
        &ctx,
        Args {
            tokens: vec!["linky".to_owned()],
            zap: false,
        },
    )
    .await;
    assert!(result.is_err(), "symlinked record must refuse");
    assert!(runner.calls().is_empty(), "no host command may run");
    assert!(fixture.env.home.join("one.txt").exists());
}

fn _path(_path: &Utf8Path) {}
