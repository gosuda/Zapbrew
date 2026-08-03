#![cfg(unix)]

use std::ffi::OsString;
use std::io;
use std::process::ExitStatus;
use std::sync::Mutex;

use camino::{Utf8Path, Utf8PathBuf};
use zapbrew_ops::OpError;
use zapbrew_ops::platform::{
    LaunchctlAction, SystemctlAction, ditto, git_clone, git_pull, hdiutil_attach, hdiutil_detach,
    installer, launchctl, pkgutil_forget, remove_path, run_checked, systemctl, unzip,
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
            "-nobrowse",
            "-readonly",
            "-mountpoint",
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
