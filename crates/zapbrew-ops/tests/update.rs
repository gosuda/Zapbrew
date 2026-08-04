#![cfg(unix)]

mod support;

use std::collections::VecDeque;
use std::fs;
use std::io;
use std::os::unix::fs::symlink;
use std::os::unix::process::ExitStatusExt;
use std::process::ExitStatus;
use std::sync::{Arc, Mutex};

use camino::{Utf8Path, Utf8PathBuf};
use support::Fixture;
use zapbrew_api::{ApiWarning, RefreshReport};
use zapbrew_ops::update_test_support;
use zapbrew_prefix::{CommandOutput, CommandRunner, CommandSpec};

#[derive(Clone)]
struct Outcome {
    success: bool,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

impl Outcome {
    fn head(value: &str) -> Self {
        Self {
            success: true,
            stdout: format!("{value}\n").into_bytes(),
            stderr: Vec::new(),
        }
    }

    fn success() -> Self {
        Self {
            success: true,
            stdout: Vec::new(),
            stderr: Vec::new(),
        }
    }

    fn failure(message: &str) -> Self {
        Self {
            success: false,
            stdout: Vec::new(),
            stderr: format!("{message}\n").into_bytes(),
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

fn report(formulae_changed: usize, casks_changed: bool) -> RefreshReport {
    RefreshReport {
        formulae_changed,
        casks_changed,
        warnings: Vec::new(),
    }
}

fn tap(fixture: &Fixture, name: &str) -> Utf8PathBuf {
    let (user, repository) = name.split_once('/').expect("tap name");
    let path = fixture
        .env
        .library
        .join("Taps")
        .join(user)
        .join(format!("homebrew-{repository}"));
    fs::create_dir_all(path.join(".git")).expect("tap git directory");
    path
}

fn rev_parse(path: &Utf8Path) -> Vec<String> {
    vec![
        "git".to_owned(),
        "-C".to_owned(),
        path.to_string(),
        "rev-parse".to_owned(),
        "HEAD".to_owned(),
    ]
}

fn pull(path: &Utf8Path) -> Vec<String> {
    vec![
        "git".to_owned(),
        "-C".to_owned(),
        path.to_string(),
        "pull".to_owned(),
        "--ff-only".to_owned(),
        "--quiet".to_owned(),
    ]
}

#[test]
fn reports_payload_combinations_pluralization_and_offline_warnings_exactly() {
    let fixture = Fixture::new();
    let (ctx, reporter) = fixture.context(Vec::new());

    update_test_support::run_with_report(&ctx, &report(0, false)).expect("current");
    assert_eq!(
        reporter.take(),
        ["ohai:Updating Homebrew...", "print:Already up-to-date."]
    );

    update_test_support::run_with_report(&ctx, &report(1, false)).expect("one formula");
    assert_eq!(
        reporter.take(),
        [
            "ohai:Updating Homebrew...",
            "ohai:Updated Formulae",
            "print:Updated 1 formula.",
        ]
    );

    update_test_support::run_with_report(&ctx, &report(3, false)).expect("formulae");
    assert_eq!(
        reporter.take(),
        [
            "ohai:Updating Homebrew...",
            "ohai:Updated Formulae",
            "print:Updated 3 formulae.",
        ]
    );

    update_test_support::run_with_report(&ctx, &report(0, true)).expect("cask only");
    assert_eq!(reporter.take(), ["ohai:Updating Homebrew..."]);

    let warning_report = RefreshReport {
        formulae_changed: 0,
        casks_changed: false,
        warnings: vec![ApiWarning::CacheFallback {
            file: "formula.jws.json".to_owned(),
        }],
    };
    update_test_support::run_with_report(&ctx, &warning_report).expect("warning");
    assert_eq!(
        reporter.take(),
        [
            "ohai:Updating Homebrew...",
            "opoo:formula.jws.json: update failed, falling back to cached version.",
            "print:Already up-to-date.",
        ]
    );
}

#[test]
fn one_changed_tap_records_exact_head_pull_head_argv_and_output() {
    let fixture = Fixture::new();
    let path = tap(&fixture, "acme/tools");
    let (mut ctx, reporter) = fixture.context(Vec::new());
    let runner = Arc::new(ScriptRunner::new([
        Outcome::head("before"),
        Outcome::success(),
        Outcome::head("after"),
    ]));
    ctx.commands = runner.clone();

    update_test_support::run_with_report(&ctx, &report(0, false)).expect("update tap");

    assert_eq!(
        runner.calls(),
        [rev_parse(&path), pull(&path), rev_parse(&path)]
    );
    assert_eq!(
        reporter.take(),
        [
            "ohai:Updating Homebrew...",
            "print:Updated 1 tap (acme/tools).",
        ]
    );
}

#[test]
fn formula_and_sorted_multi_tap_changes_report_in_exact_order() {
    let fixture = Fixture::new();
    let zeta = tap(&fixture, "zeta/extra");
    let alpha = tap(&fixture, "alpha/tools");
    let stable = tap(&fixture, "middle/stable");
    let (mut ctx, reporter) = fixture.context(Vec::new());
    let runner = Arc::new(ScriptRunner::new([
        Outcome::head("a1"),
        Outcome::success(),
        Outcome::head("a2"),
        Outcome::head("same"),
        Outcome::success(),
        Outcome::head("same"),
        Outcome::head("z1"),
        Outcome::success(),
        Outcome::head("z2"),
    ]));
    ctx.commands = runner.clone();

    update_test_support::run_with_report(&ctx, &report(2, true)).expect("combined update");

    assert_eq!(
        runner.calls(),
        [
            rev_parse(&alpha),
            pull(&alpha),
            rev_parse(&alpha),
            rev_parse(&stable),
            pull(&stable),
            rev_parse(&stable),
            rev_parse(&zeta),
            pull(&zeta),
            rev_parse(&zeta),
        ]
    );
    assert_eq!(
        reporter.take(),
        [
            "ohai:Updating Homebrew...",
            "print:Updated 2 taps (alpha/tools, zeta/extra).",
            "ohai:Updated Formulae",
            "print:Updated 2 formulae.",
        ]
    );
}

#[test]
fn read_and_pull_failures_warn_and_continue_to_later_taps() {
    let fixture = Fixture::new();
    let before_failure = tap(&fixture, "a/before");
    let pull_failure = tap(&fixture, "b/pull");
    let after_failure = tap(&fixture, "c/after");
    let changed = tap(&fixture, "d/changed");
    let (mut ctx, reporter) = fixture.context(Vec::new());
    let runner = Arc::new(ScriptRunner::new([
        Outcome::failure("cannot read before"),
        Outcome::head("p1"),
        Outcome::failure("cannot pull"),
        Outcome::head("a1"),
        Outcome::success(),
        Outcome::failure("cannot read after"),
        Outcome::head("d1"),
        Outcome::success(),
        Outcome::head("d2"),
    ]));
    ctx.commands = runner.clone();

    update_test_support::run_with_report(&ctx, &report(0, false)).expect("continued update");

    assert_eq!(
        runner.calls(),
        [
            rev_parse(&before_failure),
            rev_parse(&pull_failure),
            pull(&pull_failure),
            rev_parse(&after_failure),
            pull(&after_failure),
            rev_parse(&after_failure),
            rev_parse(&changed),
            pull(&changed),
            rev_parse(&changed),
        ]
    );
    let output = reporter.take();
    assert_eq!(output.len(), 5);
    assert_eq!(output[0], "ohai:Updating Homebrew...");
    assert!(output[1].starts_with("opoo:a/before: update failed: command `git` failed"));
    assert!(output[1].ends_with("cannot read before"));
    assert!(output[2].starts_with("opoo:b/pull: update failed: command `git` failed"));
    assert!(output[2].ends_with("cannot pull"));
    assert!(output[3].starts_with("opoo:c/after: update failed: command `git` failed"));
    assert!(output[3].ends_with("cannot read after"));
    assert_eq!(output[4], "print:Updated 1 tap (d/changed).");
}

#[test]
fn symlinked_and_non_git_directories_are_skipped_without_commands() {
    let fixture = Fixture::new();
    let taps = fixture.env.library.join("Taps/acme");
    fs::create_dir_all(&taps).expect("tap user");
    fs::create_dir_all(taps.join("homebrew-no-git")).expect("non git tap");

    let outside = fixture.env.library.join("outside/homebrew-linked");
    fs::create_dir_all(outside.join(".git")).expect("outside git");
    symlink(&outside, taps.join("homebrew-linked")).expect("tap symlink");

    let linked_git = taps.join("homebrew-linked-git");
    fs::create_dir_all(&linked_git).expect("tap with linked git");
    symlink(outside.join(".git"), linked_git.join(".git")).expect("git symlink");

    let (mut ctx, reporter) = fixture.context(Vec::new());
    let runner = Arc::new(ScriptRunner::new([]));
    ctx.commands = runner.clone();
    update_test_support::run_with_report(&ctx, &report(0, false)).expect("skip invalid taps");

    assert_eq!(
        reporter.take(),
        ["ohai:Updating Homebrew...", "print:Already up-to-date."]
    );
    assert!(runner.calls().is_empty());
}
