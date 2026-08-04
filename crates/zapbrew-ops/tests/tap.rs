#![cfg(unix)]

mod support;

use std::fs;
use std::io;
use std::os::unix::fs::symlink;
use std::os::unix::process::ExitStatusExt;
use std::path::PathBuf;
use std::process::ExitStatus;
use std::sync::{Arc, Mutex};

use support::Fixture;
use zapbrew_ops::OpError;
use zapbrew_ops::tap::{self, Args};
use zapbrew_prefix::{CommandOutput, CommandRunner, CommandSpec};

#[derive(Clone)]
enum ResultKind {
    Clone(Vec<(&'static str, Vec<u8>)>),
    Failure,
}

struct CloneRunner {
    result: ResultKind,
    calls: Mutex<Vec<Vec<String>>>,
}

impl CloneRunner {
    fn new(result: ResultKind) -> Self {
        Self {
            result,
            calls: Mutex::new(Vec::new()),
        }
    }

    fn calls(&self) -> Vec<Vec<String>> {
        self.calls.lock().expect("runner lock").clone()
    }
}

impl CommandRunner for CloneRunner {
    fn run(&self, spec: &CommandSpec) -> Result<CommandOutput, io::Error> {
        let argv = std::iter::once(spec.program().to_string_lossy().into_owned())
            .chain(
                spec.arguments()
                    .iter()
                    .map(|value| value.to_string_lossy().into_owned()),
            )
            .collect::<Vec<_>>();
        self.calls.lock().expect("runner lock").push(argv);

        match &self.result {
            ResultKind::Failure => Ok(CommandOutput::new(
                ExitStatus::from_raw(17 << 8),
                Vec::new(),
                b"clone rejected\n".to_vec(),
            )),
            ResultKind::Clone(files) => {
                let destination = spec
                    .arguments()
                    .last()
                    .map(PathBuf::from)
                    .ok_or_else(|| io::Error::other("clone destination missing"))?;
                fs::create_dir_all(&destination)?;
                for (relative, contents) in files {
                    let path = destination.join(relative);
                    if let Some(parent) = path.parent() {
                        fs::create_dir_all(parent)?;
                    }
                    fs::write(path, contents)?;
                }
                Ok(CommandOutput::new(
                    ExitStatus::from_raw(0),
                    Vec::new(),
                    Vec::new(),
                ))
            }
        }
    }
}

#[tokio::test]
async fn no_name_lists_only_real_taps_in_sorted_order() {
    let fixture = Fixture::new();
    let taps = fixture.env.library.join("Taps");
    fs::create_dir_all(taps.join("Homebrew/homebrew-zeta")).expect("official tap");
    fs::create_dir_all(taps.join("acme/homebrew-tools")).expect("third-party tap");
    fs::create_dir_all(taps.join("acme/not-a-tap")).expect("unrelated directory");
    let target = fixture.env.library.join("outside/homebrew-hidden");
    fs::create_dir_all(&target).expect("symlink target");
    symlink(
        target.parent().expect("target user"),
        taps.join("linked-user"),
    )
    .expect("user symlink");
    symlink(&target, taps.join("acme/homebrew-linked")).expect("tap symlink");
    let (ctx, reporter) = fixture.context(Vec::new());

    tap::run(&ctx, Args::default()).await.expect("list taps");

    assert_eq!(reporter.take(), ["print:acme/tools", "print:homebrew/zeta"]);
}

#[tokio::test]
async fn core_and_cask_require_force_before_any_output_or_git_call() {
    let fixture = Fixture::new();
    let (mut ctx, reporter) = fixture.context(Vec::new());
    let runner = Arc::new(CloneRunner::new(ResultKind::Clone(Vec::new())));
    ctx.commands = runner.clone();

    for raw in ["homebrew/core", "Homebrew/homebrew-cask"] {
        let error = tap::run(
            &ctx,
            Args {
                name: Some(raw.to_owned()),
                url: None,
                force: false,
            },
        )
        .await
        .expect_err("API tap refusal");
        let expected_name = if raw.ends_with("core") {
            "homebrew/core"
        } else {
            "homebrew/cask"
        };
        assert_eq!(
            error.to_string(),
            format!(
                "Tapping {expected_name} is no longer typically necessary.\nAdd --force if you are sure you need it for contributing to Homebrew."
            )
        );
    }
    assert!(reporter.take().is_empty());
    assert!(runner.calls().is_empty());
}

#[tokio::test]
async fn forced_core_and_third_party_clones_record_exact_default_and_custom_argv() {
    let fixture = Fixture::new();
    let (mut ctx, reporter) = fixture.context(Vec::new());
    let core_runner = Arc::new(CloneRunner::new(ResultKind::Clone(vec![(
        "Formula/a.rb",
        vec![b'x'; 1_000],
    )])));
    ctx.commands = core_runner.clone();

    tap::run(
        &ctx,
        Args {
            name: Some("homebrew/core".to_owned()),
            url: None,
            force: true,
        },
    )
    .await
    .expect("force core tap");
    let core_path = fixture
        .env
        .library
        .join("Taps/Homebrew/homebrew-core")
        .to_string();
    assert_eq!(
        core_runner.calls(),
        [vec![
            "git".to_owned(),
            "-c".to_owned(),
            "core.hooksPath=/dev/null".to_owned(),
            "clone".to_owned(),
            "--origin=origin".to_owned(),
            "--template=".to_owned(),
            "--config".to_owned(),
            "core.fsmonitor=false".to_owned(),
            "--end-of-options".to_owned(),
            "https://github.com/Homebrew/homebrew-core".to_owned(),
            core_path,
        ]]
    );
    assert_eq!(
        reporter.take(),
        [
            "ohai:Tapping homebrew/core".to_owned(),
            "print:Tapped (1 files, 1KB).".to_owned(),
        ]
    );

    let custom_runner = Arc::new(CloneRunner::new(ResultKind::Clone(vec![
        ("Formula/a.rb", vec![b'a'; 1_000]),
        ("cmd/brew-b", vec![b'b'; 500]),
    ])));
    ctx.commands = custom_runner.clone();
    tap::run(
        &ctx,
        Args {
            name: Some("Acme/homebrew-Tools".to_owned()),
            url: Some("ssh://git@example.test/acme/tools".to_owned()),
            force: false,
        },
    )
    .await
    .expect("custom tap");
    let custom_path = fixture
        .env
        .library
        .join("Taps/acme/homebrew-tools")
        .to_string();
    assert_eq!(
        custom_runner.calls(),
        [vec![
            "git".to_owned(),
            "-c".to_owned(),
            "core.hooksPath=/dev/null".to_owned(),
            "clone".to_owned(),
            "--origin=origin".to_owned(),
            "--template=".to_owned(),
            "--config".to_owned(),
            "core.fsmonitor=false".to_owned(),
            "--end-of-options".to_owned(),
            "ssh://git@example.test/acme/tools".to_owned(),
            custom_path,
        ]]
    );
    assert_eq!(
        reporter.take(),
        [
            "ohai:Tapping acme/tools".to_owned(),
            "print:Tapped (2 files, 1.5KB).".to_owned(),
        ]
    );
}

#[tokio::test]
async fn invalid_existing_and_failed_clones_are_stable_and_do_not_run_host_git() {
    let fixture = Fixture::new();
    let (mut ctx, reporter) = fixture.context(Vec::new());
    let runner = Arc::new(CloneRunner::new(ResultKind::Failure));
    ctx.commands = runner.clone();

    for raw in ["acme", "/repo", "user/", "a/b/c", "acme/homebrew-"] {
        let error = tap::run(
            &ctx,
            Args {
                name: Some(raw.to_owned()),
                url: None,
                force: false,
            },
        )
        .await
        .expect_err("invalid tap");
        assert_eq!(error.to_string(), format!("Invalid tap name: '{raw}'"));
    }
    assert!(runner.calls().is_empty());

    let existing = fixture.env.library.join("Taps/acme/homebrew-existing");
    fs::create_dir_all(&existing).expect("existing tap");
    let error = tap::run(
        &ctx,
        Args {
            name: Some("acme/existing".to_owned()),
            url: None,
            force: false,
        },
    )
    .await
    .expect_err("existing tap refusal");
    assert_eq!(error.to_string(), "Tap acme/existing already tapped.");
    assert!(runner.calls().is_empty());

    let error = tap::run(
        &ctx,
        Args {
            name: Some("acme/failing".to_owned()),
            url: None,
            force: false,
        },
    )
    .await
    .expect_err("clone failure");
    assert!(matches!(error, OpError::CommandFailed { .. }));
    assert_eq!(reporter.take(), ["ohai:Tapping acme/failing"]);
    assert_eq!(runner.calls().len(), 1);
}
