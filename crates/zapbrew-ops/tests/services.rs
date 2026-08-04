#![cfg(unix)]

mod support;

use std::collections::VecDeque;
use std::fs;
use std::io;
use std::os::unix::process::ExitStatusExt;
use std::process::ExitStatus;
use std::sync::{Arc, Mutex};

use serde_json::{Value, json};
use support::{Fixture, formula, write};
use zapbrew_ops::services::{self, Args, ServiceAction};
use zapbrew_ops::services_test_support;
use zapbrew_prefix::{CommandOutput, CommandRunner, CommandSpec};
use zapbrew_types::BottleTag;

#[derive(Clone)]
struct Outcome {
    success: bool,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

impl Outcome {
    fn success() -> Self {
        Self {
            success: true,
            stdout: Vec::new(),
            stderr: Vec::new(),
        }
    }

    fn stdout(value: &str) -> Self {
        Self {
            success: true,
            stdout: value.as_bytes().to_vec(),
            stderr: Vec::new(),
        }
    }

    fn failure() -> Self {
        Self {
            success: false,
            stdout: Vec::new(),
            stderr: b"inactive\n".to_vec(),
        }
    }
}

struct ScriptRunner {
    outcomes: Mutex<VecDeque<Outcome>>,
    calls: Mutex<Vec<Vec<String>>>,
}

impl ScriptRunner {
    fn new(outcomes: impl IntoIterator<Item = Outcome>) -> Self {
        Self {
            outcomes: Mutex::new(outcomes.into_iter().collect()),
            calls: Mutex::new(Vec::new()),
        }
    }

    fn calls(&self) -> Vec<Vec<String>> {
        self.calls.lock().expect("runner lock").clone()
    }
}

impl CommandRunner for ScriptRunner {
    fn run(&self, spec: &CommandSpec) -> Result<CommandOutput, io::Error> {
        self.calls.lock().expect("runner lock").push(argv(spec));
        let outcome = self
            .outcomes
            .lock()
            .expect("outcome lock")
            .pop_front()
            .ok_or_else(|| io::Error::other("unexpected command"))?;
        let raw = if outcome.success { 0 } else { 1 << 8 };
        Ok(CommandOutput::new(
            ExitStatus::from_raw(raw),
            outcome.stdout,
            outcome.stderr,
        ))
    }
}

fn argv(spec: &CommandSpec) -> Vec<String> {
    std::iter::once(spec.program().to_string_lossy().into_owned())
        .chain(
            spec.arguments()
                .iter()
                .map(|value| value.to_string_lossy().into_owned()),
        )
        .collect()
}

fn service_formula(name: &str, service: Value) -> Value {
    let mut value = formula(name, "1.0", 0);
    value["service"] = service;
    value
}

fn install(fixture: &Fixture, name: &str) {
    fixture.keg(name, "1.0", 0);
}

fn run_args(action: ServiceAction, names: &[&str]) -> Args {
    Args {
        action,
        names: names.iter().map(|name| (*name).to_owned()).collect(),
    }
}

fn normalized(value: &str, fixture: &Fixture) -> String {
    value.replace(
        fixture.env.home.parent().expect("fixture root").as_str(),
        "$ROOT",
    )
}

#[test]
fn renders_exact_systemd_unit_with_sorted_environment_and_safe_argv_quoting() {
    let fixture = Fixture::new();
    let service = json!({
        "run": ["$HOMEBREW_PREFIX/opt/demo/bin/server", "two words", "quote\"slash\\", "100% ready"],
        "run_type": "immediate",
        "keep_alive": true,
        "launch_only_once": true,
        "restart_delay": 5,
        "stop_timeout": 20,
        "nice": -5,
        "working_dir": "/$HOME/work",
        "root_dir": "$HOMEBREW_PREFIX/root",
        "input_path": "$HOMEBREW_CELLAR/input",
        "log_path": "$HOMEBREW_PREFIX/var/log/demo.log",
        "error_log_path": "$HOMEBREW_PREFIX/var/log/demo.err",
        "environment_variables": {
            "ZETA": "last",
            "ALPHA": "$HOMEBREW_PREFIX/bin",
            "PERCENT": "100%",
        }
    });

    let rendered = services_test_support::render_systemd_unit(&fixture.env, "demo", &service)
        .expect("systemd unit");

    assert_eq!(
        normalized(&rendered, &fixture),
        "[Unit]\nDescription=Homebrew generated unit for demo\n\n[Install]\nWantedBy=default.target\n\n[Service]\nType=oneshot\nExecStart=\"$ROOT/prefix/opt/demo/bin/server\" \"two words\" \"quote\\\"slash\\\\\" \"100%% ready\"\nRestart=on-failure\nRestartSec=5\nTimeoutStopSec=20\nNice=-5\nWorkingDirectory=$ROOT/home/work\nRootDirectory=$ROOT/prefix/root\nStandardInput=file:$ROOT/prefix/Cellar/input\nStandardOutput=append:$ROOT/prefix/var/log/demo.log\nStandardError=append:$ROOT/prefix/var/log/demo.err\nEnvironment=\"ALPHA=$ROOT/prefix/bin\"\nEnvironment=\"PERCENT=100%%\"\nEnvironment=\"ZETA=last\"\n"
    );
}

#[test]
fn successful_exit_false_restarts_failed_systemd_services() {
    let fixture = Fixture::new();
    let service = json!({
        "run": "$HOMEBREW_PREFIX/bin/demo",
        "keep_alive": {"successful_exit": false}
    });

    let rendered = services_test_support::render_systemd_unit(&fixture.env, "demo", &service)
        .expect("systemd unit");

    assert!(rendered.contains("\nRestart=on-failure\n"));
}

#[test]
fn always_false_omits_keep_alive_settings() {
    let fixture = Fixture::new();
    let service = json!({
        "run": "$HOMEBREW_PREFIX/bin/demo",
        "keep_alive": {"always": false}
    });

    let unit = services_test_support::render_systemd_unit(&fixture.env, "demo", &service)
        .expect("systemd unit");
    let plist = services_test_support::render_launchd_plist(&fixture.env, "demo", &service)
        .expect("launchd plist");

    assert!(!unit.contains("\nRestart="));
    assert!(!plist.contains("<key>KeepAlive</key>"));
}
#[test]
fn renders_exact_interval_and_cron_timers() {
    let fixture = Fixture::new();
    let interval = json!({
        "run": "$HOMEBREW_PREFIX/bin/demo",
        "run_type": "interval",
        "interval": 15
    });
    let cron = json!({
        "run": "$HOMEBREW_PREFIX/bin/demo",
        "run_type": "cron",
        "cron": "5 3 * * 1"
    });

    assert_eq!(
        services_test_support::render_systemd_timer(&fixture.env, "demo", &interval)
            .expect("interval timer"),
        "[Unit]\nDescription=Homebrew generated timer for demo\n\n[Install]\nWantedBy=timers.target\n\n[Timer]\nUnit=homebrew.demo.service\nOnUnitActiveSec=15\n"
    );
    assert_eq!(
        services_test_support::render_systemd_timer(&fixture.env, "demo", &cron)
            .expect("cron timer"),
        "[Unit]\nDescription=Homebrew generated timer for demo\n\n[Install]\nWantedBy=timers.target\n\n[Timer]\nUnit=homebrew.demo.service\nPersistent=true\nOnCalendar=Mon *-*-* 03:05:00\n"
    );
}

#[test]
fn serializes_exact_launchd_plist_with_sorted_environment() {
    let fixture = Fixture::new();
    let service = json!({
        "run": ["$HOMEBREW_PREFIX/opt/demo/bin/server", "serve"],
        "run_type": "interval",
        "interval": 30,
        "keep_alive": true,
        "working_dir": "/$HOME/work",
        "log_path": "$HOMEBREW_PREFIX/var/log/demo.log",
        "error_log_path": "$HOMEBREW_PREFIX/var/log/demo.err",
        "environment_variables": {
            "ZETA": "last",
            "ALPHA": "$HOMEBREW_CELLAR/demo"
        }
    });

    let rendered = services_test_support::render_launchd_plist(&fixture.env, "demo", &service)
        .expect("launchd plist");
    let parsed = plist::Value::from_reader_xml(rendered.as_bytes()).expect("serialized plist");

    assert!(parsed.as_dictionary().is_some());
    assert_eq!(
        normalized(&rendered, &fixture),
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\">\n<dict>\n\t<key>EnvironmentVariables</key>\n\t<dict>\n\t\t<key>ALPHA</key>\n\t\t<string>$ROOT/prefix/Cellar/demo</string>\n\t\t<key>ZETA</key>\n\t\t<string>last</string>\n\t</dict>\n\t<key>KeepAlive</key>\n\t<true/>\n\t<key>Label</key>\n\t<string>homebrew.mxcl.demo</string>\n\t<key>LimitLoadToSessionType</key>\n\t<array>\n\t\t<string>Aqua</string>\n\t\t<string>Background</string>\n\t\t<string>LoginWindow</string>\n\t\t<string>StandardIO</string>\n\t\t<string>System</string>\n\t</array>\n\t<key>ProgramArguments</key>\n\t<array>\n\t\t<string>$ROOT/prefix/opt/demo/bin/server</string>\n\t\t<string>serve</string>\n\t</array>\n\t<key>RunAtLoad</key>\n\t<true/>\n\t<key>StandardErrorPath</key>\n\t<string>$ROOT/prefix/var/log/demo.err</string>\n\t<key>StandardOutPath</key>\n\t<string>$ROOT/prefix/var/log/demo.log</string>\n\t<key>StartInterval</key>\n\t<integer>30</integer>\n\t<key>WorkingDirectory</key>\n\t<string>$ROOT/home/work</string>\n</dict>\n</plist>"
    );
}

#[test]
fn substitutes_supported_placeholders_and_refuses_unsupported_service_shapes() {
    let fixture = Fixture::new();
    let supported = json!({
        "run": [
            "$HOMEBREW_PREFIX/opt/demo/bin/server",
            "$HOMEBREW_CELLAR/demo",
            "/$HOME/state"
        ]
    });
    assert!(services_test_support::parse(&fixture.env, "demo", &supported).is_ok());

    for (service, expected) in [
        (
            json!({"run": {"macos": ["bin/demo"]}}),
            "Formula `demo` uses an unsupported service run object.",
        ),
        (
            json!({"run": "$HOMEBREW_REPOSITORY/bin/demo"}),
            "Formula `demo` uses unsupported service placeholders.",
        ),
        (
            json!({"run": "@@HOMEBREW_PREFIX@@/bin/demo"}),
            "Formula `demo` uses unsupported service placeholders.",
        ),
        (
            json!({"run": "bin/demo", "run_type": "socket"}),
            "Formula `demo` has unsupported service run_type `socket`.",
        ),
        (
            json!({"run": 42}),
            "Formula `demo` has an unsupported service run value.",
        ),
    ] {
        assert_eq!(
            services_test_support::parse(&fixture.env, "demo", &service)
                .expect_err("unsupported service")
                .to_string(),
            expected
        );
    }
}

#[tokio::test]
async fn linux_start_writes_unit_and_uses_exact_argv_and_output() {
    let fixture = Fixture::new();
    install(&fixture, "demo");
    let service = json!({"run": ["$HOMEBREW_PREFIX/bin/demo", "serve"]});
    let (mut ctx, reporter) = fixture.context(vec![service_formula("demo", service.clone())]);
    let runner = Arc::new(ScriptRunner::new([
        Outcome::failure(),
        Outcome::success(),
        Outcome::success(),
    ]));
    ctx.commands = runner.clone();

    services::run(&ctx, run_args(ServiceAction::Start, &["demo"]))
        .await
        .expect("start");

    let unit = fixture
        .env
        .home
        .join(".config/systemd/user/homebrew.demo.service");
    assert_eq!(
        fs::read_to_string(&unit).expect("unit file"),
        services_test_support::render_systemd_unit(&fixture.env, "demo", &service)
            .expect("rendered unit")
    );
    assert_eq!(
        runner.calls(),
        [
            vec!["systemctl", "--user", "is-active", "homebrew.demo.service"],
            vec!["systemctl", "--user", "daemon-reload"],
            vec!["systemctl", "--user", "start", "homebrew.demo.service"],
        ]
    );
    assert_eq!(
        reporter.take(),
        ["ohai:Successfully started `demo` (label: homebrew.demo)"]
    );
}

#[tokio::test]
async fn timed_linux_start_writes_both_files_and_starts_timer() {
    let fixture = Fixture::new();
    install(&fixture, "demo");
    let service = json!({
        "run": "$HOMEBREW_PREFIX/bin/demo",
        "run_type": "interval",
        "interval": 10
    });
    let (mut ctx, _reporter) = fixture.context(vec![service_formula("demo", service.clone())]);
    let runner = Arc::new(ScriptRunner::new([
        Outcome::failure(),
        Outcome::success(),
        Outcome::success(),
    ]));
    ctx.commands = runner.clone();

    services::run(&ctx, run_args(ServiceAction::Start, &["demo"]))
        .await
        .expect("timed start");

    let directory = fixture.env.home.join(".config/systemd/user");
    assert!(directory.join("homebrew.demo.service").is_file());
    assert_eq!(
        fs::read_to_string(directory.join("homebrew.demo.timer")).expect("timer file"),
        services_test_support::render_systemd_timer(&fixture.env, "demo", &service)
            .expect("render timer")
    );
    assert_eq!(
        runner.calls(),
        [
            vec!["systemctl", "--user", "is-active", "homebrew.demo.timer"],
            vec!["systemctl", "--user", "daemon-reload"],
            vec!["systemctl", "--user", "start", "homebrew.demo.timer"],
        ]
    );
}

#[tokio::test]
async fn active_start_inactive_stop_and_missing_formula_have_pinned_states() {
    let fixture = Fixture::new();
    install(&fixture, "demo");
    let service = json!({"run": "$HOMEBREW_PREFIX/bin/demo"});
    let (mut ctx, reporter) = fixture.context(vec![service_formula("demo", service)]);
    let runner = Arc::new(ScriptRunner::new([Outcome::success(), Outcome::failure()]));
    ctx.commands = runner.clone();

    services::run(&ctx, run_args(ServiceAction::Start, &["demo"]))
        .await
        .expect("already active");
    services::run(&ctx, run_args(ServiceAction::Stop, &["demo"]))
        .await
        .expect("already inactive");
    let missing = services::run(&ctx, run_args(ServiceAction::Start, &["missing"]))
        .await
        .expect_err("missing install");

    assert_eq!(missing.to_string(), "Formula `missing` is not installed.");
    assert_eq!(
        runner.calls(),
        [
            vec!["systemctl", "--user", "is-active", "homebrew.demo.service"],
            vec!["systemctl", "--user", "is-active", "homebrew.demo.service"],
        ]
    );
    assert_eq!(
        reporter.take(),
        [
            "print:Service `demo` already started, use zapbrew restart demo to restart.",
            "opoo:Service `demo` is not started.",
        ]
    );
}

#[tokio::test]
async fn absent_service_data_and_universal_platform_are_named_refusals() {
    let fixture = Fixture::new();
    install(&fixture, "demo");
    let (ctx, _reporter) = fixture.context(vec![formula("demo", "1.0", 0)]);
    let error = services::run(&ctx, run_args(ServiceAction::Start, &["demo"]))
        .await
        .expect_err("missing service");
    assert_eq!(
        error.to_string(),
        "Formula `demo` has no service definition."
    );

    let (mut ctx, _reporter) = fixture.context(vec![service_formula(
        "demo",
        json!({"run": "$HOMEBREW_PREFIX/bin/demo"}),
    )]);
    ctx.env.bottle_tag = BottleTag::All;
    let error = services::run(&ctx, run_args(ServiceAction::Start, &["demo"]))
        .await
        .expect_err("universal platform");
    assert_eq!(
        error.to_string(),
        "Services are not supported for the universal bottle platform."
    );
}

#[tokio::test]
async fn linux_stop_and_restart_use_exact_stateful_argv_and_messages() {
    let fixture = Fixture::new();
    install(&fixture, "demo");
    let service = json!({"run": "$HOMEBREW_PREFIX/bin/demo"});
    let (mut ctx, reporter) = fixture.context(vec![service_formula("demo", service)]);
    let runner = Arc::new(ScriptRunner::new([
        Outcome::success(),
        Outcome::success(),
        Outcome::success(),
        Outcome::success(),
        Outcome::failure(),
        Outcome::success(),
        Outcome::success(),
    ]));
    ctx.commands = runner.clone();

    services::run(&ctx, run_args(ServiceAction::Stop, &["demo"]))
        .await
        .expect("stop");
    services::run(&ctx, run_args(ServiceAction::Restart, &["demo"]))
        .await
        .expect("restart");

    assert_eq!(
        runner.calls(),
        [
            vec!["systemctl", "--user", "is-active", "homebrew.demo.service"],
            vec!["systemctl", "--user", "stop", "homebrew.demo.service"],
            vec!["systemctl", "--user", "is-active", "homebrew.demo.service"],
            vec!["systemctl", "--user", "stop", "homebrew.demo.service"],
            vec!["systemctl", "--user", "is-active", "homebrew.demo.service"],
            vec!["systemctl", "--user", "daemon-reload"],
            vec!["systemctl", "--user", "start", "homebrew.demo.service"],
        ]
    );
    assert_eq!(
        reporter.take(),
        [
            "ohai:Successfully stopped `demo` (label: homebrew.demo)",
            "ohai:Successfully stopped `demo` (label: homebrew.demo)",
            "ohai:Successfully started `demo` (label: homebrew.demo)",
        ]
    );
}

#[tokio::test]
async fn list_is_sorted_and_reports_started_stopped_none_and_only_service_formulae() {
    let fixture = Fixture::new();
    for name in ["gamma", "alpha", "beta", "plain"] {
        install(&fixture, name);
    }
    let formulae = vec![
        service_formula("gamma", json!({"run": "bin/gamma"})),
        service_formula("alpha", json!({"run": "bin/alpha"})),
        service_formula("beta", json!({"run": "bin/beta"})),
        formula("plain", "1.0", 0),
    ];
    let (mut ctx, reporter) = fixture.context(formulae);
    let runner = Arc::new(ScriptRunner::new([
        Outcome::stdout("active\n"),
        Outcome::failure(),
        Outcome::failure(),
    ]));
    ctx.commands = runner.clone();
    let beta_file = fixture
        .env
        .home
        .join(".config/systemd/user/homebrew.beta.service");
    write(&beta_file, "unit");

    services::run(&ctx, run_args(ServiceAction::List, &["ignored"]))
        .await
        .expect("list");

    let alpha_file = fixture
        .env
        .home
        .join(".config/systemd/user/homebrew.alpha.service");
    let gamma_file = fixture
        .env
        .home
        .join(".config/systemd/user/homebrew.gamma.service");
    assert_eq!(
        reporter.take(),
        [
            "print:Name Status File".to_owned(),
            format!("print:alpha started {alpha_file}"),
            format!("print:beta stopped {beta_file}"),
            format!("print:gamma none {gamma_file}"),
        ]
    );
    assert_eq!(
        runner.calls(),
        [
            vec!["systemctl", "--user", "is-active", "homebrew.alpha.service"],
            vec!["systemctl", "--user", "is-active", "homebrew.beta.service"],
            vec!["systemctl", "--user", "is-active", "homebrew.gamma.service"],
        ]
    );
}

#[tokio::test]
async fn macos_start_stop_restart_and_list_use_exact_launchctl_argv_and_output() {
    let fixture = Fixture::new();
    install(&fixture, "demo");
    let service = json!({"run": "$HOMEBREW_PREFIX/bin/demo"});
    let (mut ctx, reporter) = fixture.context(vec![service_formula("demo", service.clone())]);
    ctx.env.bottle_tag = "arm64_sonoma".parse().expect("macOS tag");
    let runner = Arc::new(ScriptRunner::new([
        Outcome::failure(),
        Outcome::success(),
        Outcome::success(),
        Outcome::success(),
        Outcome::success(),
        Outcome::success(),
        Outcome::failure(),
        Outcome::success(),
        Outcome::failure(),
    ]));
    ctx.commands = runner.clone();

    services::run(&ctx, run_args(ServiceAction::Start, &["demo"]))
        .await
        .expect("mac start");
    let plist_path = fixture
        .env
        .home
        .join("Library/LaunchAgents/homebrew.mxcl.demo.plist");
    assert_eq!(
        fs::read_to_string(&plist_path).expect("plist file"),
        services_test_support::render_launchd_plist(&ctx.env, "demo", &service)
            .expect("render plist")
    );
    services::run(&ctx, run_args(ServiceAction::Stop, &["demo"]))
        .await
        .expect("mac stop");
    services::run(&ctx, run_args(ServiceAction::Restart, &["demo"]))
        .await
        .expect("mac restart");
    services::run(&ctx, run_args(ServiceAction::List, &[]))
        .await
        .expect("mac list");

    let path = plist_path.to_string();
    assert_eq!(
        runner.calls(),
        [
            vec!["launchctl", "list", "homebrew.mxcl.demo"],
            vec!["launchctl", "load", path.as_str()],
            vec!["launchctl", "list", "homebrew.mxcl.demo"],
            vec!["launchctl", "unload", path.as_str()],
            vec!["launchctl", "list", "homebrew.mxcl.demo"],
            vec!["launchctl", "unload", path.as_str()],
            vec!["launchctl", "list", "homebrew.mxcl.demo"],
            vec!["launchctl", "load", path.as_str()],
            vec!["launchctl", "list", "homebrew.mxcl.demo"],
        ]
    );
    assert_eq!(
        reporter.take(),
        [
            "ohai:Successfully started `demo` (label: homebrew.mxcl.demo)".to_owned(),
            "ohai:Successfully stopped `demo` (label: homebrew.mxcl.demo)".to_owned(),
            "ohai:Successfully stopped `demo` (label: homebrew.mxcl.demo)".to_owned(),
            "ohai:Successfully started `demo` (label: homebrew.mxcl.demo)".to_owned(),
            "print:Name Status File".to_owned(),
            format!("print:demo stopped {plist_path}"),
        ]
    );
}
