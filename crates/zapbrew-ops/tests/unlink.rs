mod support;

use zapbrew_ops::OpError;
use zapbrew_ops::unlink::{self, Args};
use zapbrew_pour::{LinkOptions, link};
use zapbrew_prefix::{LockGuard, Prefix, PrefixError};

use support::{Fixture, fingerprint, formula, is_symlink};

#[tokio::test]
async fn linked_keg_wins_over_newer_and_real_unlink_keeps_opt() {
    let fixture = Fixture::new();
    let linked = fixture.keg("foo", "1.0", 0);
    let newer = fixture.keg("foo", "2.0", 1);
    fixture.keg_file(&linked, "bin/foo", "old");
    fixture.keg_file(&newer, "bin/foo", "new");
    let prefix = Prefix::new(fixture.env.clone());
    link(&linked, &prefix, LinkOptions::default()).expect("initial link");
    let (ctx, reporter) = fixture.context(vec![formula("foo", "2.0", 1)]);

    unlink::run(
        &ctx,
        Args {
            names: vec!["foo".to_owned()],
            dry_run: false,
        },
    )
    .await
    .expect("unlink");

    assert_eq!(
        reporter.take(),
        [format!(
            "print:Unlinking {}... 1 symlinks removed.",
            linked.path()
        )]
    );
    assert!(!ctx.env.prefix.join("bin/foo").exists());
    assert!(is_symlink(&ctx.env.prefix.join("opt/foo")), "opt retained");
    assert!(!is_symlink(&ctx.env.linked.join("foo")));
}

#[tokio::test]
async fn dry_run_selects_max_scheme_and_is_byte_identical() {
    let fixture = Fixture::new();
    let high_pkg = fixture.keg("foo", "9.0", 0);
    let high_scheme = fixture.keg("foo", "1.0", 1);
    fixture.keg_file(&high_pkg, "bin/old", "old");
    fixture.keg_file(&high_scheme, "bin/current", "current");
    let prefix = Prefix::new(fixture.env.clone());
    link(&high_scheme, &prefix, LinkOptions::default()).expect("initial link");
    std::fs::remove_file(fixture.env.linked.join("foo")).expect("drop linked record");
    let (ctx, reporter) = fixture.context(vec![formula("foo", "1.0", 1)]);
    let before = fingerprint(&ctx.env.prefix);

    unlink::run(
        &ctx,
        Args {
            names: vec!["foo".to_owned()],
            dry_run: true,
        },
    )
    .await
    .expect("dry-run unlink");
    assert!(
        !ctx.env.locks.exists(),
        "dry-run must not create the locks directory"
    );

    assert_eq!(fingerprint(&ctx.env.prefix), before);
    assert_eq!(
        reporter.take(),
        [
            "print:Would remove:".to_owned(),
            format!("print:{}", ctx.env.prefix.join("bin/current")),
        ]
    );
}

#[tokio::test]
async fn dry_run_ignores_a_held_formula_lock_that_blocks_real_unlink() {
    let fixture = Fixture::new();
    let keg = fixture.keg("foo", "1.0", 0);
    fixture.keg_file(&keg, "bin/foo", "foo");
    let prefix = Prefix::new(fixture.env.clone());
    link(&keg, &prefix, LinkOptions::default()).expect("initial link");
    let (ctx, _reporter) = fixture.context(vec![formula("foo", "1.0", 0)]);
    let held = LockGuard::acquire(&ctx.env.locks, "foo.formula.lock").expect("hold formula lock");

    unlink::run(
        &ctx,
        Args {
            names: vec!["foo".to_owned()],
            dry_run: true,
        },
    )
    .await
    .expect("dry-run must not contend on formula lock");

    let before = fingerprint(&ctx.env.prefix);
    let error = unlink::run(
        &ctx,
        Args {
            names: vec!["foo".to_owned()],
            dry_run: false,
        },
    )
    .await
    .expect_err("real unlink must contend on formula lock");
    assert!(
        matches!(error, OpError::Prefix(PrefixError::LockBusy { .. })),
        "expected LockBusy, got: {error}"
    );
    assert_eq!(
        fingerprint(&ctx.env.prefix),
        before,
        "blocked real unlink must not mutate"
    );

    drop(held);
}

#[tokio::test]
async fn missing_keg_refusal_is_exact_and_zero_link_count_is_reported() {
    let fixture = Fixture::new();
    let empty = fixture.keg("empty", "1.0", 0);
    let prefix = Prefix::new(fixture.env.clone());
    link(&empty, &prefix, LinkOptions::default()).expect("record-only link");
    let (ctx, reporter) = fixture.context(vec![formula("empty", "1.0", 0)]);

    unlink::run(
        &ctx,
        Args {
            names: vec!["empty".to_owned()],
            dry_run: false,
        },
    )
    .await
    .expect("zero unlink");
    assert_eq!(
        reporter.take(),
        [format!(
            "print:Unlinking {}... 0 symlinks removed.",
            empty.path()
        )]
    );

    let error = unlink::run(
        &ctx,
        Args {
            names: vec!["missing".to_owned()],
            dry_run: false,
        },
    )
    .await
    .expect_err("missing keg");
    assert_eq!(
        error.to_string(),
        format!("No such keg: {}/missing", ctx.env.cellar)
    );
}

#[tokio::test]
async fn dry_run_lists_removed_and_pruned_matching_real_unlink() {
    let fixture = Fixture::new();
    let keg = fixture.keg("nested", "1.0", 0);
    fixture.keg_file(&keg, "share/man/man1/nested.1", "manual");
    fixture.keg_file(&keg, "lib/pkgconfig/nested.pc", "pc");
    let prefix = Prefix::new(fixture.env.clone());
    link(&keg, &prefix, LinkOptions::default()).expect("initial link");
    let plan = zapbrew_pour::plan_unlink(&keg, &prefix).expect("plan");
    assert!(
        plan.pruned
            .contains(&fixture.env.prefix.join("share/man/man1")),
        "nested last-link directories must be planned for pruning"
    );
    assert!(plan.pruned.contains(&fixture.env.prefix.join("share/man")));
    assert!(
        plan.pruned
            .contains(&fixture.env.prefix.join("lib/pkgconfig"))
    );

    let (ctx, reporter) = fixture.context(vec![formula("nested", "1.0", 0)]);
    let before = fingerprint(&ctx.env.prefix);

    unlink::run(
        &ctx,
        Args {
            names: vec!["nested".to_owned()],
            dry_run: true,
        },
    )
    .await
    .expect("dry-run unlink");
    assert!(
        !ctx.env.locks.exists(),
        "dry-run must not create the locks directory"
    );

    assert_eq!(
        fingerprint(&ctx.env.prefix),
        before,
        "dry-run is byte-identical"
    );
    let mut expected = vec!["print:Would remove:".to_owned()];
    expected.extend(
        plan.removed
            .iter()
            .chain(plan.pruned.iter())
            .map(|path| format!("print:{path}")),
    );
    assert_eq!(reporter.take(), expected);

    let applied = zapbrew_pour::unlink(&keg, &prefix).expect("real unlink");
    assert_eq!(applied.removed, plan.removed);
    assert_eq!(applied.pruned, plan.pruned);
    assert_eq!(
        applied.removed.iter().chain(applied.pruned.iter()).count(),
        plan.removed.len() + plan.pruned.len()
    );
}
