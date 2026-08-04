#![cfg(unix)]

mod support;

use std::fs;
use std::io;
use std::os::unix::fs::symlink;
use std::os::unix::process::ExitStatusExt;
use std::process::ExitStatus;
use std::sync::Arc;

use camino::{Utf8Path, Utf8PathBuf};
use serde_json::json;
use support::{Fixture, RecordingReporter, fingerprint, formula, is_symlink, write};
use zapbrew_ops::services::{self, Args as ServiceArgs, ServiceAction};
use zapbrew_ops::shim::{ShimAction, hint_program};
use zapbrew_ops::shim_test_support::run_with_exe;
use zapbrew_ops::upgrade::{self, Args as UpgradeArgs};
use zapbrew_prefix::{CommandOutput, CommandRunner, CommandSpec};

/// Command runner that reports every unit as active (is-active success).
struct ActiveRunner;

impl CommandRunner for ActiveRunner {
    fn run(&self, _spec: &CommandSpec) -> Result<CommandOutput, io::Error> {
        Ok(CommandOutput::new(
            ExitStatus::from_raw(0),
            Vec::new(),
            Vec::new(),
        ))
    }
}

fn root(fixture: &Fixture) -> Utf8PathBuf {
    fixture
        .env
        .home
        .parent()
        .expect("fixture root")
        .to_path_buf()
}

/// A real executable file outside the prefix, plus its canonical path.
fn make_exe(fixture: &Fixture) -> (Utf8PathBuf, Utf8PathBuf) {
    let exe = root(fixture).join("libexec/zapbrew");
    write(&exe, "#!/bin/sh\n");
    let canonical = exe.canonicalize_utf8().expect("canonical exe");
    (exe, canonical)
}

fn link_of(fixture: &Fixture) -> Utf8PathBuf {
    fixture.env.prefix.join("bin/brew")
}

fn read_link(path: &Utf8Path) -> Utf8PathBuf {
    Utf8PathBuf::from_path_buf(fs::read_link(path.as_std_path()).expect("read_link"))
        .expect("utf8 link target")
}

#[test]
fn fresh_install_creates_absolute_canonical_link() {
    let fixture = Fixture::new();
    let (ctx, reporter) = fixture.context(vec![]);
    let (exe, canonical) = make_exe(&fixture);
    let link = link_of(&fixture);

    run_with_exe(&ctx, ShimAction::Install, &exe).expect("install");

    assert!(is_symlink(&link));
    let target = read_link(&link);
    assert!(
        target.is_absolute(),
        "shim target must be absolute: {target}"
    );
    assert_eq!(target, canonical);
    assert_eq!(
        reporter.take(),
        [format!("print:Installed brew shim: {link} -> {canonical}")]
    );
}

#[test]
fn repeated_install_is_idempotent() {
    let fixture = Fixture::new();
    let (ctx, reporter) = fixture.context(vec![]);
    let (exe, canonical) = make_exe(&fixture);
    let link = link_of(&fixture);

    run_with_exe(&ctx, ShimAction::Install, &exe).expect("install");
    reporter.take();
    let before = fingerprint(&fixture.env.prefix);

    run_with_exe(&ctx, ShimAction::Install, &exe).expect("idempotent install");

    assert_eq!(
        reporter.take(),
        [format!("print:brew shim already installed at {link}")]
    );
    assert_eq!(read_link(&link), canonical);
    assert_eq!(fingerprint(&fixture.env.prefix), before);
}

#[test]
fn install_refuses_foreign_link_without_touching_it() {
    let fixture = Fixture::new();
    let (ctx, _reporter) = fixture.context(vec![]);
    let (exe, _canonical) = make_exe(&fixture);
    let link = link_of(&fixture);
    let foreign = root(&fixture).join("elsewhere/tool");
    write(&foreign, "foreign");

    fs::create_dir_all(link.parent().expect("bin parent").as_std_path()).expect("bin");
    symlink(foreign.as_std_path(), link.as_std_path()).expect("foreign link");
    let before = fingerprint(&fixture.env.prefix);

    let err = run_with_exe(&ctx, ShimAction::Install, &exe).expect_err("foreign refusal");
    let message = err.to_string();
    assert!(message.contains(link.as_str()), "{message}");
    assert!(message.contains(foreign.as_str()), "{message}");
    assert_eq!(read_link(&link), foreign);
    assert_eq!(fingerprint(&fixture.env.prefix), before);
}

#[test]
fn install_refuses_dangling_link_without_touching_it() {
    let fixture = Fixture::new();
    let (ctx, _reporter) = fixture.context(vec![]);
    let (exe, _canonical) = make_exe(&fixture);
    let link = link_of(&fixture);
    let missing = root(&fixture).join("gone");

    fs::create_dir_all(link.parent().expect("bin parent").as_std_path()).expect("bin");
    symlink(missing.as_std_path(), link.as_std_path()).expect("dangling link");
    let before = fingerprint(&fixture.env.prefix);

    let err = run_with_exe(&ctx, ShimAction::Install, &exe).expect_err("dangling refusal");
    let message = err.to_string();
    assert!(message.contains(link.as_str()), "{message}");
    assert!(message.contains(missing.as_str()), "{message}");
    assert!(is_symlink(&link));
    assert_eq!(read_link(&link), missing);
    assert_eq!(fingerprint(&fixture.env.prefix), before);
}

#[test]
fn install_refuses_regular_file_without_touching_it() {
    let fixture = Fixture::new();
    let (ctx, _reporter) = fixture.context(vec![]);
    let (exe, _canonical) = make_exe(&fixture);
    let link = link_of(&fixture);

    write(&link, "not a shim");
    let before = fingerprint(&fixture.env.prefix);

    let err = run_with_exe(&ctx, ShimAction::Install, &exe).expect_err("regular file refusal");
    assert!(err.to_string().contains(link.as_str()));
    assert!(!is_symlink(&link));
    assert_eq!(
        fs::read_to_string(link.as_std_path()).expect("read link file"),
        "not a shim"
    );
    assert_eq!(fingerprint(&fixture.env.prefix), before);
}

#[test]
fn remove_missing_link_is_idempotent() {
    let fixture = Fixture::new();
    let (ctx, reporter) = fixture.context(vec![]);
    let (exe, _canonical) = make_exe(&fixture);
    let link = link_of(&fixture);

    run_with_exe(&ctx, ShimAction::Remove, &exe).expect("missing remove");

    assert!(!link.exists());
    assert_eq!(
        reporter.take(),
        [format!("print:No brew shim installed at {link}")]
    );
}

#[test]
fn remove_matching_link_unlinks_and_reports() {
    let fixture = Fixture::new();
    let (ctx, reporter) = fixture.context(vec![]);
    let (exe, canonical) = make_exe(&fixture);
    let link = link_of(&fixture);

    run_with_exe(&ctx, ShimAction::Install, &exe).expect("install");
    reporter.take();

    run_with_exe(&ctx, ShimAction::Remove, &exe).expect("matching remove");

    assert!(!link.exists());
    assert!(canonical.exists(), "executable must survive removal");
    assert_eq!(
        reporter.take(),
        [format!("print:Removed brew shim: {link}")]
    );
}

#[test]
fn remove_relative_matching_link_unlinks() {
    let fixture = Fixture::new();
    let (ctx, reporter) = fixture.context(vec![]);
    // Executable inside the prefix so a relative link target resolves to it.
    let exe = fixture.env.prefix.join("zapbrew");
    write(&exe, "#!/bin/sh\n");
    let link = link_of(&fixture);
    fs::create_dir_all(link.parent().expect("bin parent").as_std_path()).expect("bin");
    symlink("../zapbrew", link.as_std_path()).expect("relative link");

    run_with_exe(&ctx, ShimAction::Remove, &exe).expect("relative matching remove");

    assert!(!link.exists());
    assert!(exe.exists());
    assert_eq!(
        reporter.take(),
        [format!("print:Removed brew shim: {link}")]
    );
}

#[test]
fn remove_refuses_foreign_link_and_leaves_it() {
    let fixture = Fixture::new();
    let (ctx, _reporter) = fixture.context(vec![]);
    let (exe, _canonical) = make_exe(&fixture);
    let link = link_of(&fixture);
    let foreign = root(&fixture).join("elsewhere/tool");
    write(&foreign, "foreign");
    fs::create_dir_all(link.parent().expect("bin parent").as_std_path()).expect("bin");
    symlink(foreign.as_std_path(), link.as_std_path()).expect("foreign link");
    let before = fingerprint(&fixture.env.prefix);

    let err = run_with_exe(&ctx, ShimAction::Remove, &exe).expect_err("foreign remove refusal");
    let message = err.to_string();
    assert!(message.contains(link.as_str()), "{message}");
    assert!(message.contains(foreign.as_str()), "{message}");
    assert_eq!(read_link(&link), foreign);
    assert_eq!(fingerprint(&fixture.env.prefix), before);
}

#[test]
fn refuses_symlinked_prefix() {
    let mut fixture = Fixture::new();
    let real = root(&fixture).join("real-prefix");
    fs::create_dir_all(real.as_std_path()).expect("real prefix");
    let symlinked = root(&fixture).join("linked-prefix");
    symlink(real.as_std_path(), symlinked.as_std_path()).expect("prefix symlink");
    fixture.env.prefix = symlinked.clone();
    let (ctx, _reporter) = fixture.context(vec![]);
    let exe = root(&fixture).join("libexec/zapbrew");
    write(&exe, "#!/bin/sh\n");

    let err = run_with_exe(&ctx, ShimAction::Install, &exe).expect_err("symlinked prefix");
    assert!(err.to_string().contains(symlinked.as_str()));
    // No bin was created inside the real prefix.
    assert!(!real.join("bin").exists());
}

#[test]
fn refuses_symlinked_bin() {
    let fixture = Fixture::new();
    let (ctx, _reporter) = fixture.context(vec![]);
    let (exe, _canonical) = make_exe(&fixture);
    let elsewhere = root(&fixture).join("elsewhere");
    fs::create_dir_all(elsewhere.as_std_path()).expect("elsewhere");
    let bin = fixture.env.prefix.join("bin");
    symlink(elsewhere.as_std_path(), bin.as_std_path()).expect("bin symlink");

    let err = run_with_exe(&ctx, ShimAction::Install, &exe).expect_err("symlinked bin");
    assert!(err.to_string().contains(bin.as_str()));
    // The foreign directory the bin symlink points at gained no brew entry.
    assert!(!elsewhere.join("brew").exists());
}

#[test]
fn argv0_basename_maps_to_hint_program() {
    assert_eq!(hint_program("brew"), "brew");
    assert_eq!(hint_program("/opt/homebrew/bin/brew"), "brew");
    assert_eq!(hint_program("-brew"), "brew");
    assert_eq!(hint_program("zapbrew"), "zapbrew");
    assert_eq!(hint_program("/usr/local/bin/zapbrew"), "zapbrew");
    assert_eq!(hint_program("brew-wrapper"), "zapbrew");
    assert_eq!(hint_program("--brew"), "zapbrew");
    assert_eq!(hint_program(""), "zapbrew");
}

#[tokio::test]
async fn brew_hint_reporter_prefixes_reinstall_and_restart() {
    let fixture = Fixture::new();
    fixture.keg("upd", "1.0", 0);
    fixture.keg("svc", "1.0", 0);
    let mut service = formula("svc", "1.0", 0);
    service["service"] = json!({"run": "$HOMEBREW_PREFIX/bin/svc"});
    let brew = Arc::new(RecordingReporter::with_hint("brew"));
    let mut ctx =
        fixture.context_with_reporter(vec![formula("upd", "1.0", 0), service], brew.clone());

    upgrade::run(
        &ctx,
        UpgradeArgs {
            names: vec!["upd".to_owned()],
            dry_run: true,
        },
    )
    .await
    .expect("up-to-date");
    assert_eq!(
        brew.take(),
        [
            "opoo:upd 1.0 is already installed and up-to-date.\nTo reinstall 1.0, run:\n  brew reinstall upd"
        ]
    );

    ctx.commands = Arc::new(ActiveRunner);
    services::run(
        &ctx,
        ServiceArgs {
            action: ServiceAction::Start,
            names: vec!["svc".to_owned()],
        },
    )
    .await
    .expect("already active");
    assert_eq!(
        brew.take(),
        ["print:Service `svc` already started, use brew restart svc to restart."]
    );
}
