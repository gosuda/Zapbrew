mod support;

use std::collections::BTreeMap;
use std::fs::{self, FileTimes};
use std::io;
use std::os::unix::fs::symlink;
use std::os::unix::process::ExitStatusExt;
use std::process::ExitStatus;
use std::sync::{Arc, Mutex};
use std::time::{Duration, UNIX_EPOCH};

use zapbrew_ops::config_test_support;
use zapbrew_prefix::{CommandOutput, CommandRunner, CommandSpec};

use support::Fixture;

#[derive(Default)]
struct RecordingRunner {
    calls: Mutex<Vec<Vec<String>>>,
    fail: bool,
    origin: Option<&'static str>,
}

impl RecordingRunner {
    fn failing() -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            fail: true,
            origin: None,
        }
    }

    fn credentialed_origin() -> Self {
        Self {
            calls: Mutex::new(Vec::new()),
            fail: false,
            origin: Some("https://user:token@example.test/zapbrew.git\n"),
        }
    }

    fn calls(&self) -> Vec<Vec<String>> {
        self.calls.lock().expect("calls lock").clone()
    }
}

impl CommandRunner for RecordingRunner {
    fn run(&self, spec: &CommandSpec) -> Result<CommandOutput, io::Error> {
        let mut call = vec![spec.program().to_string_lossy().into_owned()];
        call.extend(
            spec.arguments()
                .iter()
                .map(|arg| arg.to_string_lossy().into_owned()),
        );
        self.calls.lock().expect("calls lock").push(call.clone());
        if self.fail {
            return Ok(CommandOutput::new(
                ExitStatus::from_raw(1 << 8),
                Vec::new(),
                b"failed".to_vec(),
            ));
        }
        let stdout = match call.as_slice() {
            [program, flag] if program == "git" && flag == "--version" => "git version 2.42.0\n",
            [program, flag] if program == "curl" && flag == "--version" => {
                "curl 8.4.0 (x86_64) libcurl/8.4.0\n"
            }
            call if call.ends_with(&[
                "remote".to_owned(),
                "get-url".to_owned(),
                "origin".to_owned(),
            ]) =>
            {
                self.origin
                    .map_or("https://example.test/zapbrew.git\n", std::convert::identity)
            }
            call if call.ends_with(&["rev-parse".to_owned(), "HEAD".to_owned()]) => "deadbeef\n",
            call if call.ends_with(&[
                "log".to_owned(),
                "-1".to_owned(),
                "--format=%cd".to_owned(),
            ]) =>
            {
                "2 days ago\n"
            }
            _ => "",
        };
        Ok(CommandOutput::new(
            ExitStatus::from_raw(0),
            stdout.as_bytes().to_vec(),
            Vec::new(),
        ))
    }
}

#[test]
fn renders_exact_order_redaction_defaults_and_build_rustc() {
    let fixture = Fixture::new();
    let core = fixture.env.cache.join("api/formula.jws.json");
    fs::create_dir_all(core.parent().expect("api cache")).expect("api cache");
    fs::write(&core, "catalog").expect("core json");
    let file = fs::OpenOptions::new()
        .write(true)
        .open(&core)
        .expect("core file");
    file.set_times(FileTimes::new().set_modified(UNIX_EPOCH + Duration::from_secs(0)))
        .expect("fixed mtime");
    let (mut ctx, _reporter) = fixture.context(Vec::new());
    let runner = Arc::new(RecordingRunner::default());
    ctx.commands = runner.clone();
    let vars = BTreeMap::from([
        (
            "HOMEBREW_API_DOMAIN".to_owned(),
            "https://formulae.brew.sh/api".to_owned(),
        ),
        ("HOMEBREW_CACHE".to_owned(), ctx.env.cache.to_string()),
        ("HOMEBREW_DEBUG".to_owned(), "1".to_owned()),
        (
            "HOMEBREW_GITHUB_PACKAGES_TOKEN".to_owned(),
            "super-secret".to_owned(),
        ),
        (
            "HOMEBREW_PRIVATE_MIRROR".to_owned(),
            "https://example.test/repo?token=super-secret".to_owned(),
        ),
        ("OTHER".to_owned(), "ignored".to_owned()),
    ]);

    let lines = config_test_support::lines(&ctx, &vars, 7);

    assert_eq!(
        &lines[..11],
        &[
            "HOMEBREW_VERSION: zapbrew 0.1.0 (Homebrew 5-compatible)".to_owned(),
            "ORIGIN: https://example.test/zapbrew.git".to_owned(),
            "HEAD: deadbeef".to_owned(),
            "Last commit: 2 days ago".to_owned(),
            "Core tap JSON: 01 Jan 00:00 UTC".to_owned(),
            format!("HOMEBREW_PREFIX: {}", ctx.env.prefix),
            format!("HOMEBREW_CACHE: {}", ctx.env.cache),
            "HOMEBREW_DEBUG: set".to_owned(),
            "HOMEBREW_GITHUB_PACKAGES_TOKEN: set".to_owned(),
            "HOMEBREW_PRIVATE_MIRROR: set".to_owned(),
            lines[10].clone(),
        ]
    );
    assert!(lines[10].starts_with("Rust: rustc "));
    assert_eq!(
        lines[11],
        format!("CPU: 7-core {}-bit {}", usize::BITS, std::env::consts::ARCH)
    );
    assert_eq!(lines[12], "Git: 2.42.0 => git");
    assert_eq!(lines[13], "Curl: 8.4.0 => curl");
    assert!(
        !lines
            .iter()
            .any(|line| line.contains("super-secret") || line.contains("token="))
    );
    assert!(
        !lines
            .iter()
            .any(|line| line.starts_with("HOMEBREW_CELLAR:"))
    );
    assert_eq!(runner.calls().len(), 5);
}

#[test]
fn custom_cellar_is_printed_immediately_after_prefix() {
    let fixture = Fixture::new();
    let (mut ctx, _reporter) = fixture.context(Vec::new());
    ctx.env.cellar = ctx.env.prefix.join("custom-cellar");
    ctx.commands = Arc::new(RecordingRunner::failing());

    let lines = config_test_support::lines(&ctx, &BTreeMap::new(), 1);
    assert_eq!(lines[5], format!("HOMEBREW_PREFIX: {}", ctx.env.prefix));
    assert_eq!(lines[6], format!("HOMEBREW_CELLAR: {}", ctx.env.cellar));
    assert!(lines[7].starts_with("Rust: rustc "));
}

#[test]
fn non_git_and_tool_failures_use_stable_values_and_record_exact_argv() {
    let fixture = Fixture::new();
    let (mut ctx, _reporter) = fixture.context(Vec::new());
    let runner = Arc::new(RecordingRunner::failing());
    ctx.commands = runner.clone();

    let lines = config_test_support::lines(&ctx, &BTreeMap::new(), 2);
    assert_eq!(lines[1], "ORIGIN: (none)");
    assert_eq!(lines[2], "HEAD: (none)");
    assert_eq!(lines[3], "Last commit: never");
    assert_eq!(lines[4], "Core tap: N/A");
    assert_eq!(lines[8], "Git: N/A");
    assert_eq!(lines[9], "Curl: N/A");

    let repo = ctx.env.repository.to_string();
    assert_eq!(
        runner.calls(),
        [
            vec!["git", "-C", &repo, "remote", "get-url", "origin"],
            vec!["git", "-C", &repo, "rev-parse", "HEAD"],
            vec!["git", "-C", &repo, "log", "-1", "--format=%cd"],
            vec!["git", "--version"],
            vec!["curl", "--version"],
        ]
    );
}

#[test]
fn credentialed_origin_is_redacted() {
    let fixture = Fixture::new();
    let (mut ctx, _reporter) = fixture.context(Vec::new());
    ctx.commands = Arc::new(RecordingRunner::credentialed_origin());

    let lines = config_test_support::lines(&ctx, &BTreeMap::new(), 1);
    assert_eq!(lines[1], "ORIGIN: set");
    assert!(!lines.iter().any(|line| line.contains("user:token")));
}

#[test]
fn core_json_symlink_and_symlinked_parent_are_not_followed() {
    let fixture = Fixture::new();
    let outside_dir = fixture.env.home.join("outside-api");
    fs::create_dir_all(&outside_dir).expect("outside api");
    let outside = outside_dir.join("formula.jws.json");
    fs::write(&outside, "outside").expect("outside file");
    fs::create_dir_all(&fixture.env.cache).expect("cache");
    let api = fixture.env.cache.join("api");
    symlink(&outside_dir, &api).expect("api symlink");
    let (mut ctx, _reporter) = fixture.context(Vec::new());
    ctx.commands = Arc::new(RecordingRunner::failing());

    let lines = config_test_support::lines(&ctx, &BTreeMap::new(), 1);
    assert_eq!(lines[4], "Core tap: N/A");

    fs::remove_file(&api).expect("remove api symlink");
    fs::create_dir_all(&api).expect("api cache");
    symlink(&outside, api.join("formula.jws.json")).expect("core symlink");
    let lines = config_test_support::lines(&ctx, &BTreeMap::new(), 1);
    assert_eq!(lines[4], "Core tap: N/A");
    assert_eq!(
        fs::read_to_string(outside).expect("outside untouched"),
        "outside"
    );
}
