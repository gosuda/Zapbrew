#![cfg(unix)]

//! Per-tap advisory lock tests for untap's exclusive lock acquisition.
//!
//! These tests use raw `LockGuard` probes (not production command calls) to
//! verify untap's exclusive tap lock: shared-vs-exclusive contention,
//! `--force` still requires the lock, normalization, and lock-file
//! persistence. Production command-level lock acquisition for `tap`,
//! `install`, `reinstall`, and `upgrade` is pinned in their respective
//! operation test suites (`tests/tap.rs`, `tests/install.rs`,
//! `tests/reinstall.rs`, `tests/upgrade.rs`). All probes are deterministic —
//! never timing.

mod support;

use std::fs;
use std::os::unix::fs::symlink;

use support::Fixture;
use zapbrew_ops::OpError;
use zapbrew_ops::tap_lock_test_support;
use zapbrew_ops::untap::{self, Args as UntapArgs};
use zapbrew_prefix::{LockGuard, PrefixError};

/// Acquire a shared tap lock for `raw_tap` under `env.locks`, returning the
/// guard. Mirrors the path computed by `acquire_shared_tap_locks`.
fn shared_tap_lock(env: &zapbrew_prefix::Env, raw_tap: &str) -> LockGuard {
    let dir = tap_lock_test_support::lock_dir(&env.locks, raw_tap).expect("lock dir");
    let name = tap_lock_test_support::lock_file_name(raw_tap).expect("lock name");
    LockGuard::acquire_shared(&dir, &name).expect("shared tap lock")
}

/// Acquire an exclusive tap lock for `raw_tap` under `env.locks`.
fn exclusive_tap_lock(env: &zapbrew_prefix::Env, raw_tap: &str) -> LockGuard {
    let dir = tap_lock_test_support::lock_dir(&env.locks, raw_tap).expect("lock dir");
    let name = tap_lock_test_support::lock_file_name(raw_tap).expect("lock name");
    LockGuard::acquire(&dir, &name).expect("exclusive tap lock")
}

fn create_tap(env: &zapbrew_prefix::Env, user: &str, repo: &str) {
    let path = env
        .library
        .join("Taps")
        .join(user)
        .join(format!("homebrew-{repo}"));
    fs::create_dir_all(&path).expect("tap dir");
    fs::write(path.join("formula.rb"), "f").expect("tap file");
}

#[tokio::test]
async fn untap_rejects_symlinked_tap_lock_directory() {
    let fixture = Fixture::new();
    create_tap(&fixture.env, "acme", "tools");
    fs::create_dir_all(fixture.env.locks.join("taps")).expect("tap locks root");
    let external = fixture
        .env
        .prefix
        .parent()
        .expect("prefix parent")
        .join("external");
    fs::create_dir_all(&external).expect("external directory");
    symlink(
        external.as_std_path(),
        fixture.env.locks.join("taps/acme").as_std_path(),
    )
    .expect("symlink tap lock directory");
    let (ctx, _reporter) = fixture.context(Vec::new());

    let error = untap::run(
        &ctx,
        UntapArgs {
            names: vec!["acme/tools".to_owned()],
            force: true,
        },
    )
    .await
    .expect_err("untap must reject a symlinked tap lock directory");

    assert!(
        matches!(error, OpError::InvalidState { .. }),
        "expected InvalidState, got: {error}"
    );
    assert!(
        fixture
            .env
            .library
            .join("Taps/acme/homebrew-tools")
            .is_dir(),
        "tap tree must remain intact"
    );
    assert!(
        !external.join("homebrew-tools.tap.lock").exists(),
        "lock acquisition must not follow the symlink"
    );
}

/// Untap cannot cross the scan/remove gap while a shared lock exists. A
/// shared lock — proxying what install/reinstall/upgrade hold — must block
/// untap's exclusive acquisition. (Production install/reinstall/upgrade
/// lock acquisition is pinned in their respective test suites.)
#[tokio::test]
async fn untap_blocked_by_shared_tap_lock() {
    let fixture = Fixture::new();
    create_tap(&fixture.env, "acme", "tools");
    let (ctx, _reporter) = fixture.context(Vec::new());

    let _shared = shared_tap_lock(&fixture.env, "acme/tools");

    let error = untap::run(
        &ctx,
        UntapArgs {
            names: vec!["acme/tools".to_owned()],
            force: true,
        },
    )
    .await
    .expect_err("untap must be blocked by shared lock");

    assert!(
        matches!(error, OpError::Prefix(PrefixError::LockBusy { .. })),
        "expected LockBusy, got: {error}"
    );
    // Tap tree must be intact — untap did not cross the scan/remove gap.
    assert!(
        fixture
            .env
            .library
            .join("Taps/acme/homebrew-tools")
            .is_dir(),
        "tap tree intact after blocked untap"
    );

    drop(_shared);

    untap::run(
        &ctx,
        UntapArgs {
            names: vec!["acme/tools".to_owned()],
            force: true,
        },
    )
    .await
    .expect("untap succeeds after shared lock released");
}

/// `--force` bypasses only the dependent refusal, not the exclusive tap lock.
#[tokio::test]
async fn untap_force_still_requires_exclusive_tap_lock() {
    let fixture = Fixture::new();
    create_tap(&fixture.env, "acme", "tools");
    fixture.keg_with_tap("tool", "1.0", 0, "acme/tools");
    let (ctx, _reporter) = fixture.context(Vec::new());

    let _shared = shared_tap_lock(&fixture.env, "acme/tools");

    let error = untap::run(
        &ctx,
        UntapArgs {
            names: vec!["acme/tools".to_owned()],
            force: true,
        },
    )
    .await
    .expect_err("force untap must still be blocked by shared lock");

    assert!(
        matches!(error, OpError::Prefix(PrefixError::LockBusy { .. })),
        "expected LockBusy even with --force, got: {error}"
    );
}

/// An exclusive tap lock (proxying what `tap::run` holds) blocks `untap`
/// for the same normalized tap. (`tap::run`'s actual lock acquisition is
/// pinned in `tests/tap.rs::tap_blocked_by_shared_tap_lock`.)
#[tokio::test]
async fn tap_excludes_untap_for_same_normalized_tap() {
    let fixture = Fixture::new();
    create_tap(&fixture.env, "acme", "tools");
    let (ctx, _reporter) = fixture.context(Vec::new());

    // Hold an exclusive lock as `tap` would (same path, different case input).
    let _exclusive = exclusive_tap_lock(&fixture.env, "Acme/homebrew-tools");

    let error = untap::run(
        &ctx,
        UntapArgs {
            names: vec!["acme/tools".to_owned()],
            force: true,
        },
    )
    .await
    .expect_err("untap must be blocked by exclusive tap lock");

    assert!(
        matches!(error, OpError::Prefix(PrefixError::LockBusy { .. })),
        "expected LockBusy, got: {error}"
    );

    drop(_exclusive);

    untap::run(
        &ctx,
        UntapArgs {
            names: vec!["acme/tools".to_owned()],
            force: true,
        },
    )
    .await
    .expect("untap succeeds after exclusive lock released");
}

/// Different taps do not contend: a lock on one tap does not block operations
/// on a different tap.
#[tokio::test]
async fn different_taps_do_not_contend() {
    let fixture = Fixture::new();
    create_tap(&fixture.env, "acme", "tools");
    create_tap(&fixture.env, "acme", "other");
    let (ctx, _reporter) = fixture.context(Vec::new());

    // Hold an exclusive lock on acme/tools.
    let _lock = exclusive_tap_lock(&fixture.env, "acme/tools");

    // Untap acme/other must succeed — no contention.
    untap::run(
        &ctx,
        UntapArgs {
            names: vec!["acme/other".to_owned()],
            force: true,
        },
    )
    .await
    .expect("untap of different tap must not be blocked");

    assert!(
        !fixture
            .env
            .library
            .join("Taps/acme/homebrew-other")
            .exists(),
        "other tap removed"
    );
    assert!(
        fixture
            .env
            .library
            .join("Taps/acme/homebrew-tools")
            .is_dir(),
        "locked tap intact"
    );
}

/// Two shared tap locks — proxying what two concurrent installs would hold
/// — do not serialize each other. (Production install lock acquisition is
/// pinned in `tests/install.rs`.)
#[tokio::test]
async fn two_shared_tap_locks_do_not_serialize() {
    let fixture = Fixture::new();

    let first = shared_tap_lock(&fixture.env, "acme/tools");
    // Second shared lock on the same tap must succeed — shared locks are
    // compatible with each other.
    let second = shared_tap_lock(&fixture.env, "acme/tools");
    drop(first);
    drop(second);
}

/// Normalized tap names produce the same lock path regardless of case or
/// `homebrew-` prefix in the input.
#[tokio::test]
async fn normalized_tap_names_share_lock_path() {
    let fixture = Fixture::new();

    let path_plain =
        tap_lock_test_support::lock_path(&fixture.env.locks, "acme/tools").expect("plain path");
    let path_cased = tap_lock_test_support::lock_path(&fixture.env.locks, "Acme/homebrew-tools")
        .expect("cased path");

    assert_eq!(
        path_plain, path_cased,
        "normalized tap names must produce the same lock path"
    );
}

/// Untap acquires the exclusive tap lock: after a successful untap, the
/// advisory lock file persists on disk (flock/OFD locks release the lock
/// on Drop but do not remove the file). Its presence proves the lock was
/// acquired during untap.
#[tokio::test]
async fn untap_lock_file_persists_after_success() {
    let fixture = Fixture::new();
    create_tap(&fixture.env, "acme", "tools");
    let (ctx, _reporter) = fixture.context(Vec::new());

    untap::run(
        &ctx,
        UntapArgs {
            names: vec!["acme/tools".to_owned()],
            force: true,
        },
    )
    .await
    .expect("untap");

    // Advisory lock files are created by LockGuard::acquire but are not
    // cleaned up by Drop (the OS releases the lock, the file remains). Verify
    // the lock file exists at the expected path — this proves the lock was
    // actually acquired during untap.
    let lock_path =
        tap_lock_test_support::lock_path(&fixture.env.locks, "acme/tools").expect("lock path");
    assert!(
        lock_path.exists(),
        "tap lock file should exist after untap (advisory locks leave files)"
    );
}
