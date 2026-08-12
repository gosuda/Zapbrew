#![cfg(unix)]

use std::ffi::OsString;
use std::io;
use std::process::ExitStatus;
use std::sync::Mutex;

use camino::{Utf8Path, Utf8PathBuf};
use zapbrew_ops::OpError;
use zapbrew_ops::platform::{
    LaunchctlAction, LaunchdState, SystemctlAction, SystemdActivity, SystemdEnablement, ditto,
    git_clone, git_pull, hdiutil_attach, hdiutil_detach, installer, launchctl, pkgutil_forget,
    query_launchd_state, query_systemd_activity, query_systemd_enablement, remove_path,
    run_checked, systemctl, systemctl_is_active, systemctl_is_enabled, unzip,
};
use zapbrew_prefix::{CommandOutput, CommandRunner, CommandSpec};

fn argv(spec: &CommandSpec) -> Vec<OsString> {
    std::iter::once(spec.program().to_os_string())
        .chain(spec.arguments().iter().cloned())
        .collect()
}

fn status(code: i32) -> ExitStatus {
    use std::os::unix::process::ExitStatusExt;
    ExitStatus::from_raw(code << 8)
}

#[test]
fn git_specs_match_tap_clone_and_update_contracts() {
    let destination = Utf8Path::new("/prefix/Homebrew/Library/Taps/acme/homebrew-tools");
    assert_eq!(
        argv(&git_clone(
            "https://example.test/acme/tools.git",
            destination
        )),
        [
            "git",
            "-c",
            "core.hooksPath=/dev/null",
            "clone",
            "--origin=origin",
            "--template=",
            "--config",
            "core.fsmonitor=false",
            "--end-of-options",
            "https://example.test/acme/tools.git",
            "/prefix/Homebrew/Library/Taps/acme/homebrew-tools",
        ]
    );
    assert_eq!(
        argv(&git_pull(destination)),
        [
            "git",
            "-C",
            "/prefix/Homebrew/Library/Taps/acme/homebrew-tools",
            "pull",
            "--ff-only",
            "--quiet",
        ]
    );
}

#[test]
fn service_specs_include_platform_scoping() {
    assert_eq!(
        argv(&systemctl(
            SystemctlAction::Restart,
            "homebrew.mxcl.redis.service",
        )),
        [
            "systemctl",
            "--user",
            "restart",
            "homebrew.mxcl.redis.service",
        ]
    );
    assert_eq!(
        argv(&systemctl(
            SystemctlAction::Enable,
            "homebrew.mxcl.redis.service",
        )),
        [
            "systemctl",
            "--user",
            "enable",
            "homebrew.mxcl.redis.service",
        ]
    );
    assert_eq!(
        argv(&systemctl(
            SystemctlAction::Disable,
            "homebrew.mxcl.redis.service",
        )),
        [
            "systemctl",
            "--user",
            "disable",
            "homebrew.mxcl.redis.service",
        ]
    );
    assert_eq!(
        argv(&systemctl_is_active("homebrew.mxcl.redis.service")),
        [
            "systemctl",
            "--user",
            "is-active",
            "homebrew.mxcl.redis.service",
        ]
    );
    assert_eq!(
        argv(&systemctl_is_enabled("homebrew.mxcl.redis.service")),
        [
            "systemctl",
            "--user",
            "is-enabled",
            "homebrew.mxcl.redis.service",
        ]
    );
    assert_eq!(
        argv(&launchctl(
            LaunchctlAction::Unload,
            Utf8Path::new("/home/test/Library/LaunchAgents/homebrew.mxcl.redis.plist"),
        )),
        [
            "launchctl",
            "unload",
            "/home/test/Library/LaunchAgents/homebrew.mxcl.redis.plist",
        ]
    );
}

#[test]
fn cask_specs_are_argument_safe_and_exact() {
    let image = Utf8Path::new("/scratch/Some App.dmg");
    let mount = Utf8Path::new("/scratch/mount point");
    let app = Utf8Path::new("/scratch/mount point/Some App.app");
    let destination = Utf8Path::new("/Applications/Some App.app");

    assert_eq!(
        argv(&hdiutil_attach(image, mount)),
        [
            "hdiutil",
            "attach",
            "-plist",
            "-nobrowse",
            "-readonly",
            "-mountrandom",
            "/scratch/mount point",
            "/scratch/Some App.dmg",
        ]
    );
    assert_eq!(
        argv(&hdiutil_detach(mount)),
        ["hdiutil", "detach", "/scratch/mount point"]
    );
    assert_eq!(
        argv(&ditto(app, destination)),
        [
            "ditto",
            "/scratch/mount point/Some App.app",
            "/Applications/Some App.app",
        ]
    );
    assert_eq!(
        argv(&unzip(image, mount)),
        [
            "unzip",
            "-q",
            "/scratch/Some App.dmg",
            "-d",
            "/scratch/mount point",
        ]
    );
    assert_eq!(
        argv(&installer(Utf8Path::new("/scratch/setup.pkg"))),
        [
            "/usr/sbin/installer",
            "-pkg",
            "/scratch/setup.pkg",
            "-target",
            "/",
        ]
    );
    assert_eq!(
        argv(&pkgutil_forget("com.example.some-app")),
        ["/usr/sbin/pkgutil", "--forget", "com.example.some-app",]
    );
    assert_eq!(
        argv(&remove_path(Utf8Path::new(
            "/home/test/Library/App Support"
        ))),
        ["rm", "-rf", "--", "/home/test/Library/App Support",]
    );
}

#[derive(Clone)]
enum Response {
    Output(CommandOutput),
    Io,
}

struct RecordingRunner {
    response: Response,
    calls: Mutex<Vec<Vec<OsString>>>,
}

impl RecordingRunner {
    fn new(response: Response) -> Self {
        Self {
            response,
            calls: Mutex::new(Vec::new()),
        }
    }
}

impl CommandRunner for RecordingRunner {
    fn run(&self, spec: &CommandSpec) -> Result<CommandOutput, io::Error> {
        match self.calls.lock() {
            Ok(mut calls) => calls.push(argv(spec)),
            Err(_) => return Err(io::Error::other("recording runner lock poisoned")),
        }
        match &self.response {
            Response::Output(output) => Ok(output.clone()),
            Response::Io => Err(io::Error::new(io::ErrorKind::NotFound, "tool missing")),
        }
    }
}

fn output(code: i32, stdout: &str, stderr: &str) -> Response {
    Response::Output(CommandOutput::new(
        status(code),
        stdout.as_bytes().to_vec(),
        stderr.as_bytes().to_vec(),
    ))
}

#[test]
fn systemd_queries_accept_only_documented_states() {
    let active = RecordingRunner::new(output(0, "active\n", ""));
    assert_eq!(
        query_systemd_activity(&active, "homebrew.demo.service").expect("active"),
        SystemdActivity::Active
    );
    let inactive = RecordingRunner::new(output(3, "inactive\n", ""));
    assert_eq!(
        query_systemd_activity(&inactive, "homebrew.demo.service").expect("inactive"),
        SystemdActivity::Inactive
    );
    let enabled = RecordingRunner::new(output(0, "enabled\n", ""));
    assert_eq!(
        query_systemd_enablement(&enabled, "homebrew.demo.service").expect("enabled"),
        SystemdEnablement::Enabled
    );
    let disabled = RecordingRunner::new(output(1, "disabled\n", ""));
    assert_eq!(
        query_systemd_enablement(&disabled, "homebrew.demo.service").expect("disabled"),
        SystemdEnablement::Disabled
    );
    let absent = RecordingRunner::new(output(1, "not-found\n", ""));
    assert_eq!(
        query_systemd_enablement(&absent, "homebrew.demo.service").expect("absent"),
        SystemdEnablement::Absent
    );

    for response in [output(0, "", ""), output(1, "", "permission denied\n")] {
        let runner = RecordingRunner::new(response);
        assert!(matches!(
            query_systemd_activity(&runner, "homebrew.demo.service"),
            Err(OpError::CommandFailed { .. })
        ));
    }
}

#[test]
fn launchd_query_distinguishes_running_loaded_and_unloaded() {
    let running = RecordingRunner::new(output(
        0,
        "{\n\"Label\" = \"homebrew.mxcl.demo\";\n\"PID\" = 42;\n}\n",
        "",
    ));
    assert_eq!(
        query_launchd_state(&running, "homebrew.mxcl.demo").expect("running"),
        LaunchdState::Running
    );
    let pid_zero = RecordingRunner::new(output(
        0,
        "{\n\"Label\" = \"homebrew.mxcl.demo\";\n\"PID\" = 0;\n}\n",
        "",
    ));
    assert!(matches!(
        query_launchd_state(&pid_zero, "homebrew.mxcl.demo"),
        Err(OpError::CommandFailed { .. })
    ));
    let loaded = RecordingRunner::new(output(0, "{\n\"Label\" = \"homebrew.mxcl.demo\";\n}\n", ""));
    assert_eq!(
        query_launchd_state(&loaded, "homebrew.mxcl.demo").expect("loaded"),
        LaunchdState::LoadedInactive
    );
    let unloaded = RecordingRunner::new(output(113, "", "Could not find service\n"));
    assert_eq!(
        query_launchd_state(&unloaded, "homebrew.mxcl.demo").expect("unloaded"),
        LaunchdState::Unloaded
    );
    for response in [
        output(0, "", ""),
        output(
            0,
            "{\n\"Label\" = \"homebrew.mxcl.demo\";\n\"PID\" = nope;\n}\n",
            "",
        ),
        output(
            0,
            "{\n\"Label\" = \"homebrew.mxcl.other\";\n\"PID\" = 42;\n}\n",
            "",
        ),
        output(1, "", "permission denied\n"),
    ] {
        let runner = RecordingRunner::new(response);
        assert!(matches!(
            query_launchd_state(&runner, "homebrew.mxcl.demo"),
            Err(OpError::CommandFailed { .. })
        ));
    }
}

#[test]
fn manager_queries_map_spawn_failures_to_io() {
    let runner = RecordingRunner::new(Response::Io);
    assert!(matches!(
        query_systemd_activity(&runner, "homebrew.demo.service"),
        Err(OpError::Io { .. })
    ));
}

#[test]
fn run_checked_returns_captured_success_from_injected_runner() {
    let runner = RecordingRunner::new(Response::Output(CommandOutput::new(
        status(0),
        b"mounted\n".to_vec(),
        Vec::new(),
    )));
    let output = match run_checked(
        &runner,
        &hdiutil_attach(
            Utf8Path::new("/scratch/image.dmg"),
            Utf8Path::new("/scratch/mount"),
        ),
    ) {
        Ok(output) => output,
        Err(error) => panic!("expected success, got {error:?}"),
    };
    assert_eq!(output.stdout(), b"mounted\n");
    let calls = match runner.calls.lock() {
        Ok(calls) => calls,
        Err(_) => panic!("recording runner lock poisoned"),
    };
    assert_eq!(calls.len(), 1);
}

#[test]
fn run_checked_preserves_nonzero_status_and_stderr_context() {
    let runner = RecordingRunner::new(Response::Output(CommandOutput::new(
        status(23),
        Vec::new(),
        b"permission denied\n".to_vec(),
    )));
    let error = match run_checked(&runner, &remove_path(Utf8Path::new("/protected/path"))) {
        Ok(_) => panic!("expected command failure"),
        Err(error) => error,
    };

    match error {
        OpError::CommandFailed {
            program,
            status: exit,
            stderr,
        } => {
            assert_eq!(program, "rm");
            assert_eq!(exit, status(23).to_string());
            assert_eq!(stderr, "permission denied");
        }
        other => panic!("expected command failure, got {other:?}"),
    }
}

#[test]
fn run_checked_maps_spawn_failure_to_typed_io() {
    let runner = RecordingRunner::new(Response::Io);
    let error = match run_checked(&runner, &git_pull(Utf8Path::new("/tap"))) {
        Ok(_) => panic!("expected spawn failure"),
        Err(error) => error,
    };

    match error {
        OpError::Io {
            operation,
            path,
            source,
        } => {
            assert_eq!(operation, "run");
            assert_eq!(path, Utf8PathBuf::from("git"));
            assert_eq!(source.kind(), io::ErrorKind::NotFound);
        }
        other => panic!("expected typed IO error, got {other:?}"),
    }
}
