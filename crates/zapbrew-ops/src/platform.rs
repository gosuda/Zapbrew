use camino::Utf8Path;
use zapbrew_prefix::{CommandOutput, CommandRunner, CommandSpec};

use crate::OpError;

/// A supported systemd user-service operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemctlAction {
    Disable,
    Enable,
    Start,
    Stop,
    Restart,
}

impl SystemctlAction {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Disable => "disable",
            Self::Enable => "enable",
            Self::Start => "start",
            Self::Stop => "stop",
            Self::Restart => "restart",
        }
    }
}

/// A supported launchd plist operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchctlAction {
    Load,
    Unload,
}

impl LaunchctlAction {
    const fn as_str(self) -> &'static str {
        match self {
            Self::Load => "load",
            Self::Unload => "unload",
        }
    }
}

/// Execute a command through the injected boundary and reject nonzero status.
pub fn run_checked(
    runner: &dyn CommandRunner,
    spec: &CommandSpec,
) -> Result<CommandOutput, OpError> {
    let program = spec.program().to_string_lossy().into_owned();
    let output = runner
        .run(spec)
        .map_err(|source| OpError::io("run", program.as_str(), source))?;
    if output.success() {
        return Ok(output);
    }

    let stderr = String::from_utf8_lossy(output.stderr()).trim().to_owned();
    Err(OpError::CommandFailed {
        program,
        status: output.status().to_string(),
        stderr: if stderr.is_empty() {
            "no stderr output".to_owned()
        } else {
            stderr
        },
    })
}

/// `git clone` for a third-party tap, with hooks and templates disabled.
pub fn git_clone(remote: &str, destination: &Utf8Path) -> CommandSpec {
    CommandSpec::new("git")
        .args(["-c", "core.hooksPath=/dev/null", "clone"])
        .args([
            "--origin=origin",
            "--template=",
            "--config",
            "core.fsmonitor=false",
            "--end-of-options",
        ])
        .arg(remote)
        .arg(destination.as_str())
}

/// Fast-forward a checked-out tap without interactive output.
pub fn git_pull(tap: &Utf8Path) -> CommandSpec {
    CommandSpec::new("git")
        .arg("-C")
        .arg(tap.as_str())
        .args(["pull", "--ff-only", "--quiet"])
}

/// Operate on a systemd user unit.
pub fn systemctl(action: SystemctlAction, unit: &str) -> CommandSpec {
    CommandSpec::new("systemctl")
        .arg("--user")
        .arg(action.as_str())
        .arg(unit)
}

/// Reload systemd's user-unit definitions.
pub fn systemctl_daemon_reload() -> CommandSpec {
    CommandSpec::new("systemctl")
        .arg("--user")
        .arg("daemon-reload")
}

/// Probe whether a systemd user unit is active.
pub fn systemctl_is_active(unit: &str) -> CommandSpec {
    CommandSpec::new("systemctl")
        .arg("--user")
        .arg("is-active")
        .arg(unit)
}

/// Probe whether a systemd user unit is enabled.
pub fn systemctl_is_enabled(unit: &str) -> CommandSpec {
    CommandSpec::new("systemctl")
        .arg("--user")
        .arg("is-enabled")
        .arg(unit)
}

/// Observed activity of a systemd user unit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemdActivity {
    Active,
    Inactive,
}

/// Observed enablement of a systemd user unit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemdEnablement {
    Absent,
    Enabled,
    Disabled,
}

/// Observed registration and process state of a launchd label.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LaunchdState {
    Unloaded,
    LoadedInactive,
    Running,
}

/// Query systemd activity without conflating manager failures with inactivity.
pub fn query_systemd_activity(
    runner: &dyn CommandRunner,
    unit: &str,
) -> Result<SystemdActivity, OpError> {
    let spec = systemctl_is_active(unit);
    let output = run_query(runner, &spec)?;
    let state = String::from_utf8_lossy(output.stdout()).trim().to_owned();
    match (output.status().code(), state.as_str()) {
        (Some(0), "active") => Ok(SystemdActivity::Active),
        (Some(3), "inactive" | "failed") => Ok(SystemdActivity::Inactive),
        _ => Err(query_failed(&spec, &output)),
    }
}

/// Query systemd enablement without conflating manager failures with disabled units.
pub fn query_systemd_enablement(
    runner: &dyn CommandRunner,
    unit: &str,
) -> Result<SystemdEnablement, OpError> {
    let spec = systemctl_is_enabled(unit);
    let output = run_query(runner, &spec)?;
    let state = String::from_utf8_lossy(output.stdout()).trim().to_owned();
    match (output.status().code(), state.as_str()) {
        (Some(0), "enabled" | "enabled-runtime" | "linked" | "linked-runtime" | "alias") => {
            Ok(SystemdEnablement::Enabled)
        }
        (
            Some(1),
            "disabled" | "static" | "indirect" | "generated" | "transient" | "masked"
            | "masked-runtime",
        ) => Ok(SystemdEnablement::Disabled),
        (Some(1), "not-found") => Ok(SystemdEnablement::Absent),
        _ => Err(query_failed(&spec, &output)),
    }
}

/// Query a launchd label without conflating lookup failures with an unloaded job.
pub fn query_launchd_state(
    runner: &dyn CommandRunner,
    label: &str,
) -> Result<LaunchdState, OpError> {
    let spec = CommandSpec::new("launchctl").arg("list").arg(label);
    let output = run_query(runner, &spec)?;
    if output.success() {
        let stdout = String::from_utf8_lossy(output.stdout());
        let observed_label = stdout.lines().find_map(|line| {
            line.trim()
                .strip_prefix("\"Label\" =")
                .map(|value| value.trim().trim_end_matches(';').trim_matches('"'))
        });
        if observed_label != Some(label) {
            return Err(query_failed(&spec, &output));
        }
        let pid = stdout.lines().find_map(|line| {
            line.trim()
                .strip_prefix("\"PID\" =")
                .map(|value| value.trim().trim_end_matches(';'))
        });
        return match pid {
            None | Some("-") => Ok(LaunchdState::LoadedInactive),
            Some(value) if value.parse::<u32>().is_ok_and(|pid| pid > 0) => {
                Ok(LaunchdState::Running)
            }
            Some(_) => Err(query_failed(&spec, &output)),
        };
    }

    let stderr = String::from_utf8_lossy(output.stderr());
    if output.status().code() == Some(113)
        && (stderr.contains("Could not find service") || stderr.contains("Service cannot be found"))
    {
        return Ok(LaunchdState::Unloaded);
    }
    Err(query_failed(&spec, &output))
}

fn run_query(runner: &dyn CommandRunner, spec: &CommandSpec) -> Result<CommandOutput, OpError> {
    let program = spec.program().to_string_lossy().into_owned();
    runner
        .run(spec)
        .map_err(|source| OpError::io("run", program, source))
}

fn query_failed(spec: &CommandSpec, output: &CommandOutput) -> OpError {
    let stderr = String::from_utf8_lossy(output.stderr()).trim().to_owned();
    let stdout = String::from_utf8_lossy(output.stdout()).trim().to_owned();
    OpError::CommandFailed {
        program: spec.program().to_string_lossy().into_owned(),
        status: output.status().to_string(),
        stderr: if stderr.is_empty() {
            format!("unexpected query output: {stdout:?}")
        } else {
            stderr
        },
    }
}

/// Load or unload a launchd plist.
pub fn launchctl(action: LaunchctlAction, plist: &Utf8Path) -> CommandSpec {
    CommandSpec::new("launchctl")
        .arg(action.as_str())
        .arg(plist.as_str())
}

/// Start a launchd job by label without registering it for boot.
pub fn launchctl_start(label: &str) -> CommandSpec {
    CommandSpec::new("launchctl").arg("start").arg(label)
}

/// Stop a launchd job by label without unregistering it.
pub fn launchctl_stop(label: &str) -> CommandSpec {
    CommandSpec::new("launchctl").arg("stop").arg(label)
}

/// Install a staged macOS package onto the root volume.
pub fn installer(package: &Utf8Path) -> CommandSpec {
    CommandSpec::new("/usr/sbin/installer")
        .arg("-pkg")
        .arg(package.as_str())
        .args(["-target", "/"])
}

/// Forget a macOS package receipt.
pub fn pkgutil_forget(package_id: &str) -> CommandSpec {
    CommandSpec::new("/usr/sbin/pkgutil")
        .arg("--forget")
        .arg(package_id)
}

/// Attach a disk image read-only under an explicit scratch mount root.
pub fn hdiutil_attach(image: &Utf8Path, temp_root: &Utf8Path) -> CommandSpec {
    CommandSpec::new("hdiutil")
        .args(["attach", "-plist", "-nobrowse", "-readonly", "-mountrandom"])
        .arg(temp_root.as_str())
        .arg(image.as_str())
}

/// Detach a mounted disk image.
pub fn hdiutil_detach(mountpoint: &Utf8Path) -> CommandSpec {
    CommandSpec::new("hdiutil")
        .arg("detach")
        .arg(mountpoint.as_str())
}

/// Copy a macOS bundle or directory while preserving its metadata.
pub fn ditto(source: &Utf8Path, destination: &Utf8Path) -> CommandSpec {
    CommandSpec::new("ditto")
        .arg(source.as_str())
        .arg(destination.as_str())
}

/// Extract a zip archive into an explicit staging directory.
pub fn unzip(archive: &Utf8Path, destination: &Utf8Path) -> CommandSpec {
    CommandSpec::new("unzip")
        .arg("-q")
        .arg(archive.as_str())
        .arg("-d")
        .arg(destination.as_str())
}

/// Remove a preflight-approved cask path recursively.
pub fn remove_path(path: &Utf8Path) -> CommandSpec {
    CommandSpec::new("rm")
        .args(["-rf", "--"])
        .arg(path.as_str())
}
