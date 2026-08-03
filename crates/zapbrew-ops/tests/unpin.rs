mod support;

use std::fs;
use std::os::unix::fs::symlink;

use zapbrew_ops::unpin::{self, Args};
use zapbrew_prefix::pin;

use support::{Fixture, formula};

#[tokio::test]
async fn removes_live_and_stale_pins_and_prunes_empty_directory() {
    let fixture = Fixture::new();
    let foo = fixture.keg("foo", "1.0", 0);
    fixture.keg("bar", "1.0", 0);
    pin(&fixture.env.pins, &foo).expect("live pin");
    symlink("../../../Cellar/bar/missing", fixture.env.pins.join("bar")).expect("stale pin");
    let (ctx, reporter) = fixture.context(vec![formula("foo", "1.0", 0), formula("bar", "1.0", 0)]);

    unpin::run(
        &ctx,
        Args {
            names: vec!["foo".to_owned(), "bar".to_owned()],
        },
    )
    .await
    .expect("unpin");

    assert_eq!(reporter.take(), Vec::<String>::new());
    assert!(fs::symlink_metadata(ctx.env.pins.join("foo")).is_err());
    assert!(fs::symlink_metadata(ctx.env.pins.join("bar")).is_err());
    assert!(!ctx.env.pins.exists(), "empty pins directory pruned");
}

#[tokio::test]
async fn warns_for_installed_unpinned_and_reports_missing_without_error() {
    let fixture = Fixture::new();
    fixture.keg("foo", "1.0", 0);
    let (ctx, reporter) = fixture.context(vec![formula("foo", "1.0", 0)]);

    unpin::run(
        &ctx,
        Args {
            names: vec!["missing".to_owned(), "foo".to_owned()],
        },
    )
    .await
    .expect("warnings return success");

    assert_eq!(
        reporter.take(),
        ["opoo:foo not pinned", "onoe:missing not installed"]
    );
}
