mod support;

use zapbrew_ops::unlink::{self, Args};
use zapbrew_pour::{LinkOptions, link};
use zapbrew_prefix::Prefix;

use support::{Fixture, fingerprint, formula, is_symlink, write};

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
    write(&ctx.env.locks.join("foo.formula.lock"), "");
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
