//! Integration tests for the subprocess runner seam.

use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::io;
use std::path::PathBuf;
use std::process::ExitStatus;
use std::sync::Mutex;

use zapbrew_prefix::{CommandOutput, CommandRunner, CommandSpec, SystemCommandRunner};

#[derive(Debug, Clone, PartialEq, Eq)]
struct RecordedCommand {
    program: OsString,
    args: Vec<OsString>,
    cwd: Option<PathBuf>,
    env: HashMap<OsString, OsString>,
}

impl From<&CommandSpec> for RecordedCommand {
    fn from(spec: &CommandSpec) -> Self {
        Self {
            program: spec.program().to_os_string(),
            args: spec.arguments().to_vec(),
            cwd: spec.working_dir().map(PathBuf::from),
            env: spec.environment().clone(),
        }
    }
}

struct RecordingCommandRunner {
    records: Mutex<Vec<RecordedCommand>>,
    response: CommandOutput,
}

impl RecordingCommandRunner {
    fn new(response: CommandOutput) -> Self {
        Self {
            records: Mutex::new(Vec::new()),
            response,
        }
    }

    fn take_records(&self) -> Vec<RecordedCommand> {
        match self.records.lock() {
            Ok(mut guard) => {
                let records = guard.clone();
                guard.clear();
                records
            }
            Err(_) => panic!("recording runner lock poisoned"),
        }
    }
}

impl CommandRunner for RecordingCommandRunner {
    fn run(&self, spec: &CommandSpec) -> Result<CommandOutput, io::Error> {
        match self.records.lock() {
            Ok(mut guard) => guard.push(RecordedCommand::from(spec)),
            Err(_) => return Err(io::Error::other("recording runner lock poisoned")),
        }
        Ok(self.response.clone())
    }
}

fn exit_status(code: i32) -> ExitStatus {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        ExitStatus::from_raw(code << 8)
    }
    #[cfg(not(unix))]
    {
        let _ = code;
        panic!("exit_status helper is only implemented on Unix targets");
    }
}

fn run_ok(runner: &dyn CommandRunner, spec: &CommandSpec) -> CommandOutput {
    match runner.run(spec) {
        Ok(output) => output,
        Err(err) => panic!("expected runner to succeed, got {err}"),
    }
}

#[test]
fn command_spec_fluent_builders_expose_fields() {
    let cwd = std::env::temp_dir().join("zapbrew-command-spec-cwd");
    let spec = CommandSpec::new("sw_vers")
        .arg("-productVersion")
        .args(["-buildVersion"])
        .cwd(&cwd)
        .env("HOMEBREW_PREFIX", "/opt/homebrew")
        .envs([("HOMEBREW_DEBUG", "1"), ("HOMEBREW_VERBOSE", "1")]);

    assert_eq!(spec.program(), OsStr::new("sw_vers"));
    assert_eq!(
        spec.arguments(),
        [
            OsString::from("-productVersion"),
            OsString::from("-buildVersion"),
        ]
    );
    assert_eq!(spec.working_dir(), Some(cwd.as_path()));
    assert_eq!(
        spec.environment()
            .get(OsStr::new("HOMEBREW_PREFIX"))
            .cloned(),
        Some(OsString::from("/opt/homebrew"))
    );
    assert_eq!(
        spec.environment()
            .get(OsStr::new("HOMEBREW_DEBUG"))
            .cloned(),
        Some(OsString::from("1"))
    );
    assert_eq!(
        spec.environment()
            .get(OsStr::new("HOMEBREW_VERBOSE"))
            .cloned(),
        Some(OsString::from("1"))
    );
}

#[test]
#[cfg(unix)]
fn recording_runner_preserves_non_utf8_os_strings() {
    use std::os::unix::ffi::OsStringExt;

    let program = OsString::from_vec(vec![b'c', 0xff, b'm', b'd']);
    let arg = OsString::from_vec(vec![0xfe, 0xff]);
    let key = OsString::from_vec(vec![0x80]);
    let value = OsString::from_vec(vec![0x81, 0x82]);

    let response = CommandOutput::new(exit_status(0), Vec::new(), Vec::new());
    let runner = RecordingCommandRunner::new(response);
    let spec = CommandSpec::new(program.clone())
        .arg(arg.clone())
        .env(key.clone(), value.clone());

    let _ = run_ok(&runner, &spec);
    let records = runner.take_records();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].program, program);
    assert_eq!(records[0].args, vec![arg]);
    assert_eq!(records[0].cwd, None);
    assert_eq!(records[0].env.get(&key).cloned(), Some(value));
}

#[test]
fn recording_runner_captures_program_argv_cwd_and_env() {
    let cwd = std::env::temp_dir().join("zapbrew-recording-runner");
    let response = CommandOutput::new(exit_status(0), b"ok".to_vec(), Vec::new());
    let runner = RecordingCommandRunner::new(response);

    let spec = CommandSpec::new("sw_vers")
        .arg("-productVersion")
        .cwd(&cwd)
        .env("HOMEBREW_PREFIX", "/tmp/prefix");

    let output = run_ok(&runner, &spec);
    assert!(output.success());
    assert_eq!(output.stdout(), b"ok");

    let records = runner.take_records();
    assert_eq!(records.len(), 1);
    assert_eq!(
        records[0],
        RecordedCommand {
            program: OsString::from("sw_vers"),
            args: vec![OsString::from("-productVersion")],
            cwd: Some(cwd),
            env: HashMap::from([(
                OsString::from("HOMEBREW_PREFIX"),
                OsString::from("/tmp/prefix"),
            )]),
        }
    );
}

#[test]
fn command_output_helpers_report_status_and_streams() {
    let output = CommandOutput::new(
        exit_status(17),
        b"stdout bytes".to_vec(),
        b"stderr bytes".to_vec(),
    );

    assert!(!output.success());
    assert_eq!(output.status().code(), Some(17));
    assert_eq!(output.stdout(), b"stdout bytes");
    assert_eq!(output.stderr(), b"stderr bytes");
}

#[test]
fn recording_runner_is_object_safe() {
    let response = CommandOutput::new(exit_status(0), Vec::new(), Vec::new());
    let runner = RecordingCommandRunner::new(response);
    let object_safe: Box<dyn CommandRunner> = Box::new(runner);
    let spec = CommandSpec::new("echo").arg("hi");
    let output = run_ok(object_safe.as_ref(), &spec);
    assert!(output.success());
}

#[test]
#[cfg(unix)]
fn system_runner_propagates_nonzero_status_and_captured_streams() {
    let runner = SystemCommandRunner;
    let spec = CommandSpec::new("sh").args([
        "-c",
        "printf 'stdout-line\\n'; printf 'stderr-line\\n' 1>&2; exit 42",
    ]);

    let output = run_ok(&runner, &spec);
    assert!(!output.success());
    assert_eq!(output.stdout(), b"stdout-line\n");
    assert_eq!(output.stderr(), b"stderr-line\n");
    assert_eq!(output.status().code(), Some(42));
}
