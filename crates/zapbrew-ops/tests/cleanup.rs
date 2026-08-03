mod support;

use std::collections::BTreeSet;
use std::fs;
use std::os::unix::fs::symlink;

use serde_json::json;
use zapbrew_ops::cleanup::{self, Args};
use zapbrew_ops::{cleanup_test_support, transaction_test_support};
use zapbrew_pour::{LinkOptions, link};
use zapbrew_prefix::{LockGuard, Prefix, pin};

use support::{Fixture, fingerprint, formula, write};

#[tokio::test]
async fn dry_run_lists_sorted_sizes_without_mutating_or_touching_marker() {
    let fixture = Fixture::new();
    fs::create_dir_all(&fixture.env.cache).expect("cache");
    fs::write(
        fixture.env.cache.join("c.incomplete"),
        vec![0_u8; 1_000_000],
    )
    .expect("1MB");
    fs::write(fixture.env.cache.join("a.incomplete"), vec![0_u8; 999]).expect("999B");
    fs::write(fixture.env.cache.join("b.incomplete"), vec![0_u8; 1000]).expect("1KB");
    let before = fingerprint(fixture.env.cache.parent().expect("cache parent"));
    let (ctx, reporter) = fixture.context(Vec::new());

    cleanup::run(
        &ctx,
        Args {
            dry_run: true,
            ..Args::default()
        },
    )
    .await
    .expect("dry cleanup");

    assert_eq!(
        before,
        fingerprint(ctx.env.cache.parent().expect("cache parent"))
    );
    assert_eq!(
        reporter.take(),
        [
            format!("print:Would remove: {}/a.incomplete (999B)", ctx.env.cache),
            format!("print:Would remove: {}/b.incomplete (1KB)", ctx.env.cache),
            format!("print:Would remove: {}/c.incomplete (1MB)", ctx.env.cache),
            "ohai:This operation would free approximately 1MB of disk space.".to_owned(),
        ]
    );
    assert!(!ctx.env.cache.join(".cleaned").exists());
}

#[tokio::test]
async fn keeps_current_linked_and_pinned_kegs_while_selecting_old_and_old_scheme() {
    let fixture = Fixture::new();
    let old = fixture.keg("plain", "1.0", 0);
    fixture.keg_file(&old, "old", "old");
    fixture.keg("plain", "2.0", 0);

    let linked = fixture.keg("linked", "1.0", 0);
    fixture.keg_file(&linked, "bin/linked", "linked");
    fixture.keg("linked", "2.0", 0);
    link(
        &linked,
        &Prefix::new(fixture.env.clone()),
        LinkOptions::default(),
    )
    .expect("link old keg");

    let pinned = fixture.keg("pinned", "1.0", 0);
    fixture.keg("pinned", "2.0", 0);
    pin(&fixture.env.pins, &pinned).expect("pin old keg");

    let scheme = fixture.keg("scheme", "2.0", 0);
    fixture.keg_file(&scheme, "old-scheme", "x");
    let (ctx, reporter) = fixture.context(vec![
        formula("plain", "2.0", 0),
        formula("linked", "2.0", 0),
        formula("pinned", "2.0", 0),
        formula("scheme", "2.0", 1),
    ]);

    cleanup::run(
        &ctx,
        Args {
            dry_run: true,
            ..Args::default()
        },
    )
    .await
    .expect("candidate cleanup");
    let output = reporter.take();
    assert!(
        output
            .iter()
            .any(|line| line.contains(old.path().as_str())
                && line.starts_with("print:Would remove:"))
    );
    assert!(output.iter().any(
        |line| line.contains(scheme.path().as_str()) && line.starts_with("print:Would remove:")
    ));
    assert!(output.iter().any(|line| line
        == &format!(
            "opoo:Skipping (old) {} due to it being linked",
            linked.path()
        )));
    assert!(output.iter().any(|line| line
        == &format!(
            "opoo:Skipping (old) {} due to it being pinned",
            pinned.path()
        )));
    assert!(!output.iter().any(|line| line.contains("/plain/2.0 (")));
}

#[tokio::test]
async fn named_alias_scope_and_no_cleanup_formulae_are_honored() {
    let mut fixture = Fixture::new();
    fixture.keg("foo", "1.0", 0);
    fixture.keg("foo", "2.0", 0);
    fixture.keg("bar", "1.0", 0);
    fixture.keg("bar", "2.0", 0);
    fixture.env.no_cleanup_formulae = vec!["bar".to_owned()];
    let mut foo = formula("foo", "2.0", 0);
    foo["aliases"] = json!(["f"]);
    let (ctx, reporter) = fixture.context(vec![foo, formula("bar", "2.0", 0)]);

    cleanup::run(
        &ctx,
        Args {
            names: vec!["f".to_owned(), "bar".to_owned()],
            dry_run: true,
            scrub: false,
        },
    )
    .await
    .expect("named cleanup");
    let output = reporter.take();
    assert!(output.iter().any(|line| line.contains("/foo/1.0")));
    assert!(!output.iter().any(|line| line.contains("/bar/1.0")));
    assert!(output.iter().any(|line| line
        == "onoe:Refusing to clean bar because it is listed in HOMEBREW_NO_CLEANUP_FORMULAE!"));
}

#[tokio::test]
async fn catalog_latest_missing_warns_and_keeps_every_installed_keg() {
    let fixture = Fixture::new();
    fixture.keg("foo", "1.0", 0);
    fixture.keg("foo", "2.0", 0);
    let (ctx, reporter) = fixture.context(vec![formula("foo", "3.0", 0)]);

    cleanup::run(
        &ctx,
        Args {
            dry_run: true,
            ..Args::default()
        },
    )
    .await
    .expect("missing latest cleanup");
    assert_eq!(
        reporter.take(),
        ["opoo:Skipping foo: most recent version 3.0 not installed"]
    );
}

#[test]
fn age_requires_both_mtime_and_ctime_strictly_before_cutoff() {
    assert!(cleanup_test_support::older_than(9, 9, 10, 0));
    assert!(!cleanup_test_support::older_than(9, 10, 10, 0));
    assert!(!cleanup_test_support::older_than(10, 9, 10, 0));
    assert!(!cleanup_test_support::older_than(9, 9, 86_409, 1));
    assert!(cleanup_test_support::older_than(8, 8, 86_409, 1));
}

#[tokio::test]
async fn scrub_keeps_catalog_latest_and_every_installed_version() {
    let fixture = Fixture::new();
    fixture.keg("foo", "1.0", 0);
    fixture.keg("foo", "2.0", 0);
    fs::create_dir_all(fixture.env.cache.join("downloads")).expect("downloads");
    for version in ["0.5", "1.0", "2.0"] {
        write(
            &fixture.env.cache.join(format!(
                "downloads/foo--{version}.x86_64_linux.bottle.tar.gz"
            )),
            version,
        );
    }
    let (ctx, reporter) = fixture.context(vec![formula("foo", "2.0", 0)]);

    cleanup::run(
        &ctx,
        Args {
            dry_run: true,
            scrub: true,
            ..Args::default()
        },
    )
    .await
    .expect("scrub cleanup");
    let output = reporter.take();
    assert!(
        output
            .iter()
            .any(|line| line.contains("foo--0.5.x86_64_linux.bottle.tar.gz"))
    );
    assert!(
        !output
            .iter()
            .any(|line| line.contains("foo--1.0.x86_64_linux.bottle.tar.gz"))
    );
    assert!(
        !output
            .iter()
            .any(|line| line.contains("foo--2.0.x86_64_linux.bottle.tar.gz"))
    );
}

#[tokio::test]
async fn valid_alias_protects_target_and_outside_alias_is_removed_without_following() {
    let fixture = Fixture::new();
    let downloads = fixture.env.cache.join("downloads");
    fs::create_dir_all(&downloads).expect("downloads");
    write(&downloads.join("kept"), "kept");
    let outside = fixture.env.home.join("outside");
    write(&outside, "outside");
    symlink("downloads/kept", fixture.env.cache.join("valid")).expect("valid alias");
    symlink(&outside, fixture.env.cache.join("outside-alias")).expect("outside alias");
    let (mut ctx, reporter) = fixture.context(Vec::new());
    ctx.env.cleanup_max_age_days = 0;

    cleanup::run(
        &ctx,
        Args {
            dry_run: true,
            ..Args::default()
        },
    )
    .await
    .expect("alias cleanup");
    let output = reporter.take();
    assert!(!output.iter().any(|line| line.contains("downloads/kept")));
    assert!(!output.iter().any(|line| line.contains("/valid (")));
    assert!(output.iter().any(|line| line.contains("outside-alias")));
    assert_eq!(
        fs::read_to_string(outside).expect("outside survives"),
        "outside"
    );
}

#[tokio::test]
async fn stale_lock_is_candidate_but_busy_lock_stays() {
    let fixture = Fixture::new();
    fs::create_dir_all(&fixture.env.locks).expect("locks");
    write(&fixture.env.locks.join("stale.lock"), "s");
    write(&fixture.env.locks.join("busy.lock"), "b");
    let busy = LockGuard::acquire(&fixture.env.locks, "busy.lock").expect("busy guard");
    let (ctx, reporter) = fixture.context(Vec::new());

    cleanup::run(
        &ctx,
        Args {
            dry_run: true,
            ..Args::default()
        },
    )
    .await
    .expect("lock cleanup");
    let output = reporter.take();
    assert!(output.iter().any(|line| line.contains("stale.lock")));
    assert!(!output.iter().any(|line| line.contains("busy.lock")));
    cleanup::run(&ctx, Args::default())
        .await
        .expect("real lock cleanup");
    assert!(!ctx.env.locks.join("stale.lock").exists());
    assert!(ctx.env.locks.join("busy.lock").exists());
    drop(busy);
}

#[tokio::test]
async fn real_cleanup_removes_broken_links_and_owned_empty_directories_then_touches_marker() {
    let fixture = Fixture::new();
    let empty = fixture.env.prefix.join("share/nested/empty");
    fs::create_dir_all(&empty).expect("empty dirs");
    symlink("missing", fixture.env.prefix.join("share/nested/broken")).expect("broken link");
    let (ctx, reporter) = fixture.context(Vec::new());

    cleanup::run(&ctx, Args::default())
        .await
        .expect("real cleanup");
    assert!(fs::symlink_metadata(ctx.env.prefix.join("share/nested/broken")).is_err());
    assert!(!ctx.env.prefix.join("share/nested").exists());
    assert!(ctx.env.cache.join(".cleaned").is_file());
    assert!(reporter.take().is_empty());
}

#[tokio::test]
async fn failure_is_aggregated_after_other_candidates_are_processed() {
    let fixture = Fixture::new();
    let old = fixture.keg("foo", "1.0", 0);
    fixture.keg_file(&old, "payload", "payload");
    fixture.keg("foo", "2.0", 0);
    write(&fixture.env.cache.join("other.incomplete"), "other");
    let (ctx, _reporter) = fixture.context(vec![formula("foo", "2.0", 0)]);
    transaction_test_support::fail_next_backup_cleanup("foo").expect("arm cleanup failure");

    let error = cleanup::run(&ctx, Args::default())
        .await
        .expect_err("aggregate cleanup failure");
    assert!(
        error
            .to_string()
            .contains(".zapbrew-cellar-uninstall-trash")
    );
    assert!(!ctx.env.cache.join("other.incomplete").exists());
    assert!(!ctx.env.cache.join(".cleaned").exists());
}

#[tokio::test]
async fn symlinked_prefix_parent_is_rejected_without_traversing_target() {
    let fixture = Fixture::new();
    let outside = fixture.env.home.join("outside-var");
    let linked = outside.join("homebrew/linked");
    fs::create_dir_all(&linked).expect("outside linked");
    let broken = linked.join("broken");
    symlink("missing", &broken).expect("outside broken link");
    symlink(&outside, fixture.env.prefix.join("var")).expect("prefix var symlink");
    let (ctx, _reporter) = fixture.context(Vec::new());

    let error = cleanup::run(
        &ctx,
        Args {
            dry_run: true,
            ..Args::default()
        },
    )
    .await
    .expect_err("symlinked prefix component");
    assert!(
        error
            .to_string()
            .contains("cleanup path has a non-directory component")
    );
    assert!(fs::symlink_metadata(broken).is_ok());
}

#[tokio::test]
async fn missing_named_formula_is_typed_and_mutates_nothing() {
    let fixture = Fixture::new();
    let before = fingerprint(fixture.env.cellar.parent().expect("prefix"));
    let (ctx, _reporter) = fixture.context(Vec::new());
    let error = cleanup::run(
        &ctx,
        Args {
            names: vec!["missing".to_owned()],
            ..Args::default()
        },
    )
    .await
    .expect_err("missing formula");
    assert_eq!(
        error.to_string(),
        "No available formula with the name \"missing\"."
    );
    assert_eq!(
        before,
        fingerprint(ctx.env.cellar.parent().expect("prefix"))
    );
}

#[test]
fn candidate_name_set_is_deterministic() {
    let names = BTreeSet::from(["b", "a"]);
    assert_eq!(names.into_iter().collect::<Vec<_>>(), ["a", "b"]);
}
