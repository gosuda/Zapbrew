#![cfg(unix)]

mod support;

use std::collections::VecDeque;
use std::fs;
use std::io;
use std::os::unix::process::ExitStatusExt;
use std::process::ExitStatus;
use std::sync::{Arc, Mutex};

use support::Fixture;
use zapbrew_ops::OpError;
use zapbrew_ops::tap_info::{self, Args};
use zapbrew_prefix::{CommandOutput, CommandRunner, CommandSpec};

#[derive(Clone)]
enum Response {
    Output(&'static str),
    Failure,
    Io,
}

struct ScriptedRunner {
    responses: Mutex<VecDeque<Response>>,
    calls: Mutex<Vec<Vec<String>>>,
}

impl ScriptedRunner {
    fn new(responses: impl IntoIterator<Item = Response>) -> Self {
        Self {
            responses: Mutex::new(responses.into_iter().collect()),
            calls: Mutex::new(Vec::new()),
        }
    }

    fn calls(&self) -> Vec<Vec<String>> {
        self.calls.lock().expect("calls lock").clone()
    }
}

impl CommandRunner for ScriptedRunner {
    fn run(&self, spec: &CommandSpec) -> Result<CommandOutput, io::Error> {
        self.calls.lock().expect("calls lock").push(
            std::iter::once(spec.program().to_string_lossy().into_owned())
                .chain(
                    spec.arguments()
                        .iter()
                        .map(|value| value.to_string_lossy().into_owned()),
                )
                .collect(),
        );
        let response = self
            .responses
            .lock()
            .expect("responses lock")
            .pop_front()
            .ok_or_else(|| io::Error::other("unexpected git command"))?;
        match response {
            Response::Output(stdout) => Ok(CommandOutput::new(
                ExitStatus::from_raw(0),
                stdout.as_bytes().to_vec(),
                Vec::new(),
            )),
            Response::Failure => Ok(CommandOutput::new(
                ExitStatus::from_raw(12 << 8),
                Vec::new(),
                b"not a repository\n".to_vec(),
            )),
            Response::Io => Err(io::Error::new(io::ErrorKind::NotFound, "git missing")),
        }
    }
}

#[tokio::test]
async fn summary_counts_real_installed_tap_content_without_git_or_ruby() {
    let fixture = Fixture::new();
    let acme = fixture.env.library.join("Taps/acme/homebrew-tools");
    let core = fixture.env.library.join("Taps/Homebrew/homebrew-core");
    fs::create_dir_all(acme.join("Formula")).expect("acme formula dir");
    fs::create_dir_all(acme.join("cmd")).expect("acme command dir");
    fs::create_dir_all(core.join("Formula")).expect("core formula dir");
    fs::create_dir_all(core.join("Casks")).expect("core cask dir");
    fs::write(acme.join("Formula/a.rb"), vec![b'a'; 1_000]).expect("formula");
    fs::write(acme.join("cmd/brew-a"), vec![b'b'; 500]).expect("command");
    fs::write(core.join("Formula/b.rb"), vec![b'c'; 500]).expect("core formula");
    fs::write(core.join("Casks/c.rb"), vec![b'd'; 500]).expect("cask");
    let (ctx, reporter) = fixture.context(Vec::new());

    tap_info::run(&ctx, Args::default())
        .await
        .expect("tap summary");

    assert_eq!(
        reporter.take(),
        ["print:2 taps, 0 private, 2 formulae, 1 commands, 2.5KB"]
    );
}

#[tokio::test]
async fn installed_flag_prints_sorted_blocks_and_records_every_git_argv() {
    let fixture = Fixture::new();
    let alpha = fixture.env.library.join("Taps/acme/homebrew-alpha");
    let zeta = fixture.env.library.join("Taps/zeta/homebrew-tools");
    fs::create_dir_all(alpha.join("Formula")).expect("formula dir");
    fs::create_dir_all(alpha.join("Casks")).expect("cask dir");
    fs::create_dir_all(alpha.join("cmd")).expect("command dir");
    fs::create_dir_all(&zeta).expect("empty tap");
    fs::write(alpha.join("Formula/a.rb"), vec![b'a'; 1_000]).expect("formula");
    fs::write(alpha.join("Casks/c.rb"), vec![b'c'; 500]).expect("cask");
    fs::write(alpha.join("cmd/brew-z"), vec![b'z'; 250]).expect("command");
    fs::write(alpha.join("README"), vec![b'r'; 250]).expect("readme");
    let runner = Arc::new(ScriptedRunner::new([
        Response::Output("ssh://git@example.test/acme/alpha\n"),
        Response::Output("0123456789abcdef\n"),
        Response::Output("2 days ago\n"),
        Response::Output("feature/work\n"),
        Response::Output("https://github.com/zeta/homebrew-tools\n"),
        Response::Output("fedcba9876543210\n"),
        Response::Output("3 weeks ago\n"),
        Response::Output("main\n"),
    ]));
    let (mut ctx, reporter) = fixture.context(Vec::new());
    ctx.commands = runner.clone();

    tap_info::run(
        &ctx,
        Args {
            names: Vec::new(),
            installed: true,
            json: false,
        },
    )
    .await
    .expect("installed tap info");

    assert_eq!(
        reporter.take(),
        [
            format!(
                "print:acme/alpha: Installed\n1 command, 1 cask, 1 formula\n{} (4 files, 2KB)\norigin: ssh://git@example.test/acme/alpha\nHEAD: 0123456789abcdef\nlast commit: 2 days ago\nbranch: feature/work",
                alpha
            ),
            "print:".to_owned(),
            format!(
                "print:zeta/tools: Installed\nNo commands/casks/formulae\n{} (0 files, 0B)\norigin: https://github.com/zeta/homebrew-tools\nHEAD: fedcba9876543210\nlast commit: 3 weeks ago",
                zeta
            ),
        ]
    );

    let expected = [
        (alpha.as_str(), vec!["config", "--get", "remote.origin.url"]),
        (alpha.as_str(), vec!["rev-parse", "HEAD"]),
        (alpha.as_str(), vec!["log", "-1", "--format=%cr"]),
        (alpha.as_str(), vec!["symbolic-ref", "--short", "HEAD"]),
        (zeta.as_str(), vec!["config", "--get", "remote.origin.url"]),
        (zeta.as_str(), vec!["rev-parse", "HEAD"]),
        (zeta.as_str(), vec!["log", "-1", "--format=%cr"]),
        (zeta.as_str(), vec!["symbolic-ref", "--short", "HEAD"]),
    ]
    .map(|(path, arguments)| {
        ["git", "-C", path]
            .into_iter()
            .chain(arguments)
            .map(str::to_owned)
            .collect::<Vec<_>>()
    });
    assert_eq!(runner.calls(), expected);
}

#[tokio::test]
async fn git_failures_use_stable_fields_and_never_fail_tap_info() {
    let fixture = Fixture::new();
    let tap = fixture.env.library.join("Taps/acme/homebrew-tools");
    fs::create_dir_all(&tap).expect("tap");
    let runner = Arc::new(ScriptedRunner::new([
        Response::Failure,
        Response::Io,
        Response::Failure,
        Response::Io,
    ]));
    let (mut ctx, reporter) = fixture.context(Vec::new());
    ctx.commands = runner.clone();

    tap_info::run(
        &ctx,
        Args {
            names: vec!["acme/tools".to_owned()],
            installed: false,
            json: false,
        },
    )
    .await
    .expect("fallback tap info");

    assert_eq!(
        reporter.take(),
        [format!(
            "print:acme/tools: Installed\nNo commands/casks/formulae\n{} (0 files, 0B)\norigin: (none)\nHEAD: (none)\nlast commit: never\nbranch: (none)",
            tap
        )]
    );
    assert_eq!(runner.calls().len(), 4);
}

#[tokio::test]
async fn named_taps_are_sorted_missing_taps_are_all_reported_and_json_refuses_early() {
    let fixture = Fixture::new();
    let alpha = fixture.env.library.join("Taps/acme/homebrew-alpha");
    fs::create_dir_all(&alpha).expect("alpha tap");
    let runner = Arc::new(ScriptedRunner::new([
        Response::Output("origin\n"),
        Response::Output("head\n"),
        Response::Output("today\n"),
        Response::Output("master\n"),
    ]));
    let (mut ctx, reporter) = fixture.context(Vec::new());
    ctx.commands = runner.clone();

    let error = tap_info::run(
        &ctx,
        Args {
            names: vec!["zeta/missing".to_owned(), "acme/alpha".to_owned()],
            installed: false,
            json: false,
        },
    )
    .await
    .expect_err("missing named tap result");
    assert!(matches!(error, OpError::Refusal { .. }));
    assert_eq!(
        reporter.take(),
        [
            format!(
                "print:acme/alpha: Installed\nNo commands/casks/formulae\n{} (0 files, 0B)\norigin: origin\nHEAD: head\nlast commit: today",
                alpha
            ),
            "print:".to_owned(),
            "print:zeta/missing: Not installed".to_owned(),
        ]
    );

    let calls_before_json = runner.calls();
    let error = tap_info::run(
        &ctx,
        Args {
            names: vec!["bad".to_owned()],
            installed: true,
            json: true,
        },
    )
    .await
    .expect_err("JSON refusal");
    assert_eq!(
        error.to_string(),
        "tap-info JSON output is unavailable without Ruby."
    );
    assert_eq!(runner.calls(), calls_before_json);
    assert!(reporter.take().is_empty());
}
