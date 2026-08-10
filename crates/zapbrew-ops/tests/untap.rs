#![cfg(unix)]

mod support;

use std::fs;
use std::os::unix::fs::symlink;

use support::Fixture;
use zapbrew_ops::OpError;
use zapbrew_ops::untap::{self, Args};

#[tokio::test]
async fn removes_taps_in_input_order_and_prunes_only_the_empty_real_user_directory() {
    let fixture = Fixture::new();
    let first = fixture.env.library.join("Taps/acme/homebrew-first");
    let second = fixture.env.library.join("Taps/acme/homebrew-second");
    fs::create_dir_all(&first).expect("first tap");
    fs::create_dir_all(&second).expect("second tap");
    fs::write(first.join("one.rb"), vec![b'a'; 1_000]).expect("first file");
    fs::write(second.join("two.rb"), vec![b'b'; 500]).expect("second file");
    let cellar_marker = fixture.env.cellar.join("keep/me");
    fs::create_dir_all(cellar_marker.parent().expect("marker parent")).expect("cellar parent");
    fs::write(&cellar_marker, "keep").expect("cellar marker");
    let (ctx, reporter) = fixture.context(Vec::new());

    untap::run(
        &ctx,
        Args {
            names: vec!["acme/first".to_owned(), "acme/second".to_owned()],
            force: true,
        },
    )
    .await
    .expect("untap both");

    assert_eq!(
        reporter.take(),
        [
            "ohai:Untapping acme/first",
            "print:Untapped (1 files, 1KB).",
            "ohai:Untapping acme/second",
            "print:Untapped (1 files, 500B).",
        ]
    );
    assert!(!first.exists());
    assert!(!second.exists());
    assert!(!fixture.env.library.join("Taps/acme").exists());
    assert_eq!(
        fs::read_to_string(cellar_marker).expect("cellar marker"),
        "keep"
    );
}

#[tokio::test]
async fn removes_symlink_entries_without_following_their_target_outside_taps() {
    let fixture = Fixture::new();
    let tap_path = fixture.env.library.join("Taps/acme/homebrew-safe");
    let outside = fixture.env.library.join("outside");
    fs::create_dir_all(&tap_path).expect("tap");
    fs::create_dir_all(&outside).expect("outside");
    fs::write(tap_path.join("payload"), vec![b'x'; 984]).expect("tap payload");
    fs::write(outside.join("keep"), "outside").expect("outside marker");
    symlink("../../../outside", tap_path.join("outside-link")).expect("outside symlink");
    let (ctx, reporter) = fixture.context(Vec::new());

    untap::run(
        &ctx,
        Args {
            names: vec!["acme/safe".to_owned()],
            force: false,
        },
    )
    .await
    .expect("untap symlink-bearing tap");

    assert_eq!(
        reporter.take(),
        ["ohai:Untapping acme/safe", "print:Untapped (2 files, 1KB).",]
    );
    assert_eq!(
        fs::read_to_string(outside.join("keep")).expect("outside marker"),
        "outside"
    );
}

#[tokio::test]
async fn missing_non_directory_and_symlink_taps_are_refused_without_traversal() {
    let fixture = Fixture::new();
    let user = fixture.env.library.join("Taps/acme");
    let target = fixture.env.library.join("outside-target");
    fs::create_dir_all(&user).expect("tap user");
    fs::create_dir_all(&target).expect("outside target");
    fs::write(target.join("keep"), "outside").expect("outside marker");
    fs::write(user.join("homebrew-file"), "not a directory").expect("file tap");
    symlink(&target, user.join("homebrew-linked")).expect("linked tap");
    let (ctx, reporter) = fixture.context(Vec::new());

    for name in ["acme/missing", "acme/file", "acme/linked"] {
        let error = untap::run(
            &ctx,
            Args {
                names: vec![name.to_owned()],
                force: false,
            },
        )
        .await
        .expect_err("missing tap refusal");
        assert_eq!(error.to_string(), format!("No available tap {name}."));
    }
    assert!(reporter.take().is_empty());
    assert_eq!(
        fs::read_to_string(target.join("keep")).expect("outside marker"),
        "outside"
    );
}

#[tokio::test]
async fn parses_every_name_before_removing_any_tap() {
    let fixture = Fixture::new();
    let tap_path = fixture.env.library.join("Taps/acme/homebrew-keep");
    fs::create_dir_all(&tap_path).expect("tap");
    let (ctx, reporter) = fixture.context(Vec::new());

    let error = untap::run(
        &ctx,
        Args {
            names: vec!["acme/keep".to_owned(), "invalid".to_owned()],
            force: false,
        },
    )
    .await
    .expect_err("invalid second name");

    assert_eq!(error.to_string(), "Invalid tap name: 'invalid'");
    assert!(tap_path.is_dir());
    assert!(reporter.take().is_empty());
}

#[tokio::test]
async fn refuses_without_force_when_a_keg_receipt_names_the_tap() {
    let fixture = Fixture::new();
    let tap_path = fixture.env.library.join("Taps/acme/homebrew-tools");
    fs::create_dir_all(&tap_path).expect("tap");
    fs::write(tap_path.join("tool.rb"), "tool").expect("tap file");
    // Deliberately non-normalized: mixed-case user, homebrew- prefix present.
    // `Acme/homebrew-tools` must match requested `acme/tools`.
    fixture.keg_with_tap("tool", "1.0", 0, "Acme/homebrew-tools");
    let (ctx, reporter) = fixture.context(Vec::new());

    let error = untap::run(
        &ctx,
        Args {
            names: vec!["acme/tools".to_owned()],
            force: false,
        },
    )
    .await
    .expect_err("dependent-formula refusal");

    assert!(
        error.to_string().contains("acme/tools"),
        "error should name the tap: {error}"
    );
    assert!(tap_path.is_dir(), "tap tree must be intact after refusal");
    assert!(reporter.take().is_empty(), "no untap output on refusal");
}

#[tokio::test]
async fn skips_malformed_receipt_and_still_refuses_a_later_valid_dependent() {
    let fixture = Fixture::new();
    let tap_path = fixture.env.library.join("Taps/acme/homebrew-tools");
    fs::create_dir_all(&tap_path).expect("tap");
    fs::write(tap_path.join("tool.rb"), "tool").expect("tap file");
    fixture.keg_with_tap("aaa-broken", "1.0", 0, "acme");
    fixture.keg_with_tap("zzz-dependent", "1.0", 0, "Acme/homebrew-tools");
    let (ctx, reporter) = fixture.context(Vec::new());

    let error = untap::run(
        &ctx,
        Args {
            names: vec!["acme/tools".to_owned()],
            force: false,
        },
    )
    .await
    .expect_err("valid dependent after malformed receipt");

    assert!(matches!(error, OpError::Refusal { .. }));
    assert_eq!(
        error.to_string(),
        "Refusing to untap acme/tools: installed formula zzz-dependent depends on it."
    );
    assert!(tap_path.is_dir(), "tap tree must be intact after refusal");
    assert_eq!(
        reporter.take(),
        ["opoo:Skipping aaa-broken: receipt names an unparseable tap `acme`"]
    );
}

#[tokio::test]
async fn untaps_when_only_a_malformed_unrelated_receipt_exists() {
    let fixture = Fixture::new();
    let tap_path = fixture.env.library.join("Taps/acme/homebrew-tools");
    fs::create_dir_all(&tap_path).expect("tap");
    fs::write(tap_path.join("tool.rb"), "tool").expect("tap file");
    fixture.keg_with_tap("aaa-broken", "1.0", 0, "acme");
    let (ctx, reporter) = fixture.context(Vec::new());

    untap::run(
        &ctx,
        Args {
            names: vec!["acme/tools".to_owned()],
            force: false,
        },
    )
    .await
    .expect("unrelated malformed receipt must not block untap");

    assert!(!tap_path.exists());
    assert!(!fixture.env.library.join("Taps/acme").exists());
    assert_eq!(
        reporter.take(),
        [
            "opoo:Skipping aaa-broken: receipt names an unparseable tap `acme`",
            "ohai:Untapping acme/tools",
            "print:Untapped (1 files, 4B).",
        ]
    );
}

#[tokio::test]
async fn removes_with_force_even_when_a_keg_receipt_names_the_tap() {
    let fixture = Fixture::new();
    let tap_path = fixture.env.library.join("Taps/acme/homebrew-tools");
    fs::create_dir_all(&tap_path).expect("tap");
    fs::write(tap_path.join("tool.rb"), "tool").expect("tap file");
    fixture.keg_with_tap("tool", "1.0", 0, "Acme/homebrew-tools");
    let (ctx, reporter) = fixture.context(Vec::new());

    untap::run(
        &ctx,
        Args {
            names: vec!["acme/tools".to_owned()],
            force: true,
        },
    )
    .await
    .expect("force untap");

    assert!(!tap_path.exists());
    assert!(!fixture.env.library.join("Taps/acme").exists());
    assert_eq!(
        reporter.take(),
        ["ohai:Untapping acme/tools", "print:Untapped (1 files, 4B)."]
    );
}
