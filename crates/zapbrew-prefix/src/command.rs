//! Subprocess execution seam: owned command specs, captured output, and a
//! testable runner trait.

use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Output};

/// Fully owned description of a subprocess to spawn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandSpec {
    program: OsString,
    args: Vec<OsString>,
    cwd: Option<PathBuf>,
    env: HashMap<OsString, OsString>,
}

impl CommandSpec {
    /// Start building a command for `program`.
    pub fn new(program: impl Into<OsString>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            cwd: None,
            env: HashMap::new(),
        }
    }

    /// Append one argument.
    pub fn arg(mut self, arg: impl Into<OsString>) -> Self {
        self.args.push(arg.into());
        self
    }

    /// Append multiple arguments.
    pub fn args(mut self, args: impl IntoIterator<Item = impl Into<OsString>>) -> Self {
        self.args.extend(args.into_iter().map(Into::into));
        self
    }

    /// Set the working directory for the child process.
    pub fn cwd(mut self, cwd: impl Into<PathBuf>) -> Self {
        self.cwd = Some(cwd.into());
        self
    }

    /// Set or override one environment variable for the child process.
    pub fn env(mut self, key: impl Into<OsString>, value: impl Into<OsString>) -> Self {
        self.env.insert(key.into(), value.into());
        self
    }

    /// Set or override multiple environment variables for the child process.
    pub fn envs(
        mut self,
        vars: impl IntoIterator<Item = (impl Into<OsString>, impl Into<OsString>)>,
    ) -> Self {
        for (key, value) in vars {
            self.env.insert(key.into(), value.into());
        }
        self
    }

    /// Program to execute.
    pub fn program(&self) -> &OsStr {
        &self.program
    }

    /// Arguments following the program name.
    pub fn arguments(&self) -> &[OsString] {
        &self.args
    }

    /// Working directory, if any.
    pub fn working_dir(&self) -> Option<&Path> {
        self.cwd.as_deref()
    }

    /// Environment overrides applied on top of the inherited process environment.
    pub fn environment(&self) -> &HashMap<OsString, OsString> {
        &self.env
    }
}

/// Captured result of a completed subprocess.
#[derive(Debug, Clone)]
pub struct CommandOutput {
    status: ExitStatus,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

impl CommandOutput {
    /// Build output from raw pieces (tests and recording runners).
    pub fn new(status: ExitStatus, stdout: Vec<u8>, stderr: Vec<u8>) -> Self {
        Self {
            status,
            stdout,
            stderr,
        }
    }

    /// Whether the process exited successfully (status code zero).
    pub fn success(&self) -> bool {
        self.status.success()
    }

    /// Exit status reported by the operating system.
    pub fn status(&self) -> ExitStatus {
        self.status
    }

    /// Captured standard output bytes.
    pub fn stdout(&self) -> &[u8] {
        &self.stdout
    }

    /// Captured standard error bytes.
    pub fn stderr(&self) -> &[u8] {
        &self.stderr
    }
}

impl From<Output> for CommandOutput {
    fn from(output: Output) -> Self {
        Self {
            status: output.status,
            stdout: output.stdout,
            stderr: output.stderr,
        }
    }
}

/// Object-safe subprocess runner used by production code and tests.
pub trait CommandRunner: Send + Sync {
    /// Spawn `spec`, wait for completion, and return captured output.
    ///
    /// Returns `Err` only when the process cannot be started or its output
    /// cannot be read. A nonzero exit status is returned as `Ok` output for
    /// callers to classify.
    fn run(&self, spec: &CommandSpec) -> Result<CommandOutput, io::Error>;
}

/// Production runner backed by [`std::process::Command`].
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct SystemCommandRunner;

impl CommandRunner for SystemCommandRunner {
    fn run(&self, spec: &CommandSpec) -> Result<CommandOutput, io::Error> {
        let mut command = Command::new(&spec.program);
        command.args(&spec.args);
        if let Some(cwd) = &spec.cwd {
            command.current_dir(cwd);
        }
        for (key, value) in &spec.env {
            command.env(key, value);
        }
        command.output().map(CommandOutput::from)
    }
}
