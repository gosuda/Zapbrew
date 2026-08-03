mod support;

use std::fs;
use std::os::unix::fs::symlink;

use zapbrew_ops::pin::{self, Args};

use support::{Fixture, formula};

#[tokio::test]
async fn pins_max_scheme_then_pkg_with_exact_relative_record() {
    let fixture = Fixture::new();
    fixture.keg("foo", "9.0", 1);
    fixture.keg("foo", "1.0", 2);
    let (ctx, reporter) = fixture.context(vec![formula("foo", "1.0", 2)]);

    pin::run(
        &ctx,
        Args {
            names: vec!["foo".to_owned()],
        },
    )
    .await
    .expect("pin");

    assert_eq!(reporter.take(), Vec::<String>::new());
    assert_eq!(
        fs::read_link(ctx.env.pins.join("foo")).expect("pin target"),
        std::path::PathBuf::from("../../../Cellar/foo/1.0")
    );
}

#[tokio::test]
async fn stale_pin_warns_and_missing_formula_refuses_without_mutation() {
    let fixture = Fixture::new();
    fixture.keg("foo", "1.0", 0);
    fs::create_dir_all(&fixture.env.pins).expect("pins");
    symlink("../../../Cellar/foo/missing", fixture.env.pins.join("foo")).expect("stale pin");
    let (ctx, reporter) = fixture.context(vec![formula("foo", "1.0", 0)]);

    pin::run(
        &ctx,
        Args {
            names: vec!["foo".to_owned()],
        },
    )
    .await
    .expect("already pinned");
    assert_eq!(reporter.take(), ["opoo:foo already pinned"]);
    assert_eq!(
        fs::read_link(ctx.env.pins.join("foo")).expect("stale retained"),
        std::path::PathBuf::from("../../../Cellar/foo/missing")
    );

    let error = pin::run(
        &ctx,
        Args {
            names: vec!["missing".to_owned()],
        },
    )
    .await
    .expect_err("missing install");
    assert_eq!(error.to_string(), "missing not installed");
    assert!(fs::symlink_metadata(ctx.env.pins.join("missing")).is_err());
}
