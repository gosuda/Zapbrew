mod support;

use std::fs;
use std::sync::Arc;

use serde_json::json;
use support::{Fixture, PanicRunner};
use zapbrew_ops::OpError;
use zapbrew_ops::cask::list::{self, Args};

fn install_dir(fixture: &Fixture, token: &str, versions: &[&str]) {
    for version in versions {
        fs::create_dir_all(fixture.env.caskroom.join(token).join(version)).expect("version");
    }
}

#[tokio::test]
async fn empty_and_sorted_column_and_one_per_line_output() {
    let fixture = Fixture::new().macos();
    let (ctx, reporter) =
        fixture.context_casks(vec![], Arc::new(PanicRunner), reqwest::Client::new());
    list::run(&ctx, Args::default()).await.expect("empty list");
    assert!(reporter.take().is_empty());

    install_dir(&fixture, "zulu", &["2.0"]);
    install_dir(&fixture, "alpha", &["1.0"]);
    install_dir(&fixture, "middle", &["3.0"]);
    list::run(
        &ctx,
        Args {
            width: 80,
            ..Args::default()
        },
    )
    .await
    .expect("column list");
    assert_eq!(
        reporter.take(),
        vec!["print:alpha                      middle                     zulu\n"]
    );

    list::run(
        &ctx,
        Args {
            one_per_line: true,
            width: 80,
            ..Args::default()
        },
    )
    .await
    .expect("one per line");
    assert_eq!(reporter.take(), vec!["print:alpha\nmiddle\nzulu\n"]);
}

#[tokio::test]
async fn versions_and_named_old_token_output() {
    let fixture = Fixture::new().macos();
    install_dir(&fixture, "modern", &["1.0", "2.0"]);
    let catalog = vec![json!({
        "token": "modern",
        "old_tokens": ["legacy"],
        "version": "2.0",
        "sha256": "no_check",
        "url": "https://example.test/modern.zip",
        "artifacts": []
    })];
    let (ctx, reporter) =
        fixture.context_casks(catalog, Arc::new(PanicRunner), reqwest::Client::new());
    list::run(
        &ctx,
        Args {
            versions: true,
            ..Args::default()
        },
    )
    .await
    .expect("versions");
    assert_eq!(reporter.take(), vec!["print:modern 1.0 2.0"]);

    list::run(
        &ctx,
        Args {
            tokens: vec!["legacy".to_owned()],
            ..Args::default()
        },
    )
    .await
    .expect("named rename");
    assert_eq!(reporter.take(), vec!["print:modern 1.0 2.0"]);
}

#[tokio::test]
async fn no_follow_token_and_version_symlinks() {
    let fixture = Fixture::new().macos();
    let outside = fixture.env.home.join("outside");
    fs::create_dir_all(outside.join("9.0")).expect("outside");
    fs::create_dir_all(&fixture.env.caskroom).expect("caskroom");
    std::os::unix::fs::symlink(&outside, fixture.env.caskroom.join("evil")).expect("token symlink");
    fs::create_dir_all(fixture.env.caskroom.join("safe")).expect("safe token");
    std::os::unix::fs::symlink(outside.join("9.0"), fixture.env.caskroom.join("safe/9.0"))
        .expect("version symlink");

    let (ctx, reporter) =
        fixture.context_casks(vec![], Arc::new(PanicRunner), reqwest::Client::new());
    list::run(&ctx, Args::default())
        .await
        .expect("no-follow list");
    assert!(reporter.take().is_empty());
}

#[tokio::test]
async fn named_missing_catalog_and_install_refuse() {
    let fixture = Fixture::new().macos();
    let catalog = vec![json!({
        "token": "known",
        "version": "1.0",
        "sha256": "no_check",
        "url": "https://example.test/known.zip",
        "artifacts": []
    })];
    let (ctx, _reporter) =
        fixture.context_casks(catalog, Arc::new(PanicRunner), reqwest::Client::new());
    let unavailable = list::run(
        &ctx,
        Args {
            tokens: vec!["ghost".to_owned()],
            ..Args::default()
        },
    )
    .await;
    assert!(
        matches!(unavailable, Err(OpError::Refusal { message }) if message == "Cask 'ghost' is unavailable.")
    );
    let uninstalled = list::run(
        &ctx,
        Args {
            tokens: vec!["known".to_owned()],
            ..Args::default()
        },
    )
    .await;
    assert!(
        matches!(uninstalled, Err(OpError::Refusal { message }) if message == "Cask 'known' is not installed.")
    );
}

#[tokio::test]
async fn linux_empty_list_output() {
    let fixture = Fixture::new();
    let (ctx, reporter) =
        fixture.context_casks(vec![], Arc::new(PanicRunner), reqwest::Client::new());
    list::run(&ctx, Args::default()).await.expect("empty list");
    assert!(reporter.take().is_empty());
}

#[tokio::test]
async fn linux_sorted_column_and_one_per_line_output() {
    let fixture = Fixture::new();
    install_dir(&fixture, "zulu", &["2.0"]);
    install_dir(&fixture, "alpha", &["1.0"]);
    install_dir(&fixture, "middle", &["3.0"]);
    let (ctx, reporter) =
        fixture.context_casks(vec![], Arc::new(PanicRunner), reqwest::Client::new());

    list::run(
        &ctx,
        Args {
            width: 80,
            ..Args::default()
        },
    )
    .await
    .expect("column list");
    assert_eq!(
        reporter.take(),
        vec!["print:alpha                      middle                     zulu\n"]
    );

    list::run(
        &ctx,
        Args {
            one_per_line: true,
            width: 80,
            ..Args::default()
        },
    )
    .await
    .expect("one per line");
    assert_eq!(reporter.take(), vec!["print:alpha\nmiddle\nzulu\n"]);
}

#[tokio::test]
async fn linux_versions_list_output() {
    let fixture = Fixture::new();
    install_dir(&fixture, "modern", &["1.0", "2.0"]);
    let (ctx, reporter) =
        fixture.context_casks(vec![], Arc::new(PanicRunner), reqwest::Client::new());
    list::run(
        &ctx,
        Args {
            versions: true,
            ..Args::default()
        },
    )
    .await
    .expect("versions");
    assert_eq!(reporter.take(), vec!["print:modern 1.0 2.0"]);
}

#[tokio::test]
async fn linux_old_token_resolves() {
    let fixture = Fixture::new();
    install_dir(&fixture, "modern", &["1.0", "2.0"]);
    let catalog = vec![json!({
        "token": "modern",
        "old_tokens": ["legacy"],
        "version": "2.0",
        "sha256": "no_check",
        "url": "https://example.test/modern.zip",
        "artifacts": []
    })];
    let (ctx, reporter) =
        fixture.context_casks(catalog, Arc::new(PanicRunner), reqwest::Client::new());
    list::run(
        &ctx,
        Args {
            tokens: vec!["legacy".to_owned()],
            ..Args::default()
        },
    )
    .await
    .expect("named rename");
    assert_eq!(reporter.take(), vec!["print:modern 1.0 2.0"]);
}
