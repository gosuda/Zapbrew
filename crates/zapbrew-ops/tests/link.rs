mod support;

use std::fs;
use std::os::unix::fs::symlink;
use std::sync::Arc;

use serde_json::json;
use zapbrew_ops::link::{self, Args};
use zapbrew_ops::unlink;
use zapbrew_pour::{LinkOptions, link as pour_link};
use zapbrew_prefix::Prefix;

use support::{
    Fixture, RecordingReporter, fingerprint, formula, is_symlink, keg_only_formula, write,
};

#[tokio::test]
async fn links_max_scheme_keg_with_records_and_exact_count_then_warns_when_repeated() {
    let fixture = Fixture::new();
    let high_pkg = fixture.keg("foo", "9.0", 0);
    let selected = fixture.keg("foo", "1.0", 1);
    fixture.keg_file(&high_pkg, "bin/wrong", "wrong");
    fixture.keg_file(&selected, "bin/foo", "selected");
    let (ctx, reporter) = fixture.context(vec![formula("foo", "1.0", 1)]);

    link::run(
        &ctx,
        Args {
            names: vec!["foo".to_owned()],
            ..Args::default()
        },
    )
    .await
    .expect("link");

    assert_eq!(
        reporter.take(),
        [format!(
            "print:Linking {}... 3 symlinks created.",
            selected.path()
        )]
    );
    assert!(is_symlink(&ctx.env.prefix.join("bin/foo")));
    assert!(!ctx.env.prefix.join("bin/wrong").exists());
    assert!(is_symlink(&ctx.env.prefix.join("opt/foo")));
    assert!(is_symlink(&ctx.env.linked.join("foo")));

    link::run(
        &ctx,
        Args {
            names: vec!["foo".to_owned()],
            ..Args::default()
        },
    )
    .await
    .expect("already linked");
    assert_eq!(
        reporter.take(),
        [
            format!("opoo:Already linked: {}", selected.path()),
            "print:To relink, run:\n  brew unlink foo && brew link foo".to_owned(),
        ]
    );
}

#[tokio::test]
async fn verbose_link_lists_created_paths_in_sorted_order() {
    let fixture = Fixture::new();
    let keg = fixture.keg("foo", "1.0", 0);
    fixture.keg_file(&keg, "bin/zeta", "zeta");
    fixture.keg_file(&keg, "bin/alpha", "alpha");
    let (mut ctx, _) = fixture.context(vec![formula("foo", "1.0", 0)]);
    let reporter = Arc::new(RecordingReporter::verbose());
    ctx.reporter = reporter.clone();

    link::run(
        &ctx,
        Args {
            names: vec!["foo".to_owned()],
            ..Args::default()
        },
    )
    .await
    .expect("link");

    assert_eq!(
        reporter.take(),
        [
            format!("print:Linking {}... 4 symlinks created.", keg.path()),
            format!("print:{}", ctx.env.prefix.join("bin/alpha")),
            format!("print:{}", ctx.env.prefix.join("bin/zeta")),
        ]
    );
}

#[tokio::test]
async fn refuses_when_another_version_is_linked() {
    let fixture = Fixture::new();
    let old = fixture.keg("foo", "1.0", 0);
    let current = fixture.keg("foo", "2.0", 1);
    fixture.keg_file(&old, "bin/foo", "old");
    fixture.keg_file(&current, "bin/foo", "current");
    pour_link(
        &old,
        &Prefix::new(fixture.env.clone()),
        LinkOptions::default(),
    )
    .expect("old link");
    let (ctx, _reporter) = fixture.context(vec![formula("foo", "2.0", 1)]);

    let error = link::run(
        &ctx,
        Args {
            names: vec!["foo".to_owned()],
            ..Args::default()
        },
    )
    .await
    .expect_err("other linked version");
    assert_eq!(
        error.to_string(),
        format!(
            "Cannot link foo\nAnother version is already linked: {}",
            old.path()
        )
    );
}

#[tokio::test]
async fn conflict_messages_cover_real_file_and_other_keg_without_mutation() {
    let fixture = Fixture::new();
    let foo = fixture.keg("foo", "1.0", 0);
    fixture.keg_file(&foo, "bin/tool", "foo");
    write(&fixture.env.prefix.join("bin/tool"), "user");
    let (ctx, _reporter) = fixture.context(vec![formula("foo", "1.0", 0)]);
    write(&ctx.env.locks.join("foo.formula.lock"), "");
    let before = fingerprint(&ctx.env.prefix);

    let error = link::run(
        &ctx,
        Args {
            names: vec!["foo".to_owned()],
            ..Args::default()
        },
    )
    .await
    .expect_err("real conflict");
    assert_eq!(fingerprint(&ctx.env.prefix), before);
    assert_eq!(
        error.to_string(),
        format!(
            "Could not symlink bin/tool\nTarget {0}/bin/tool already exists. You may want to remove it:\n  rm '{0}/bin/tool'\nTo force the link and overwrite all conflicting files:\n  brew link --overwrite foo\n\nTo list all files that would be deleted:\n  brew link --overwrite foo --dry-run",
            ctx.env.prefix
        )
    );

    fs::remove_file(ctx.env.prefix.join("bin/tool")).expect("remove user file");
    let bar = fixture.keg("bar", "1.0", 0);
    fixture.keg_file(&bar, "bin/tool", "bar");
    symlink(bar.path().join("bin/tool"), ctx.env.prefix.join("bin/tool"))
        .expect("foreign keg link");
    let error = link::run(
        &ctx,
        Args {
            names: vec!["foo".to_owned()],
            ..Args::default()
        },
    )
    .await
    .expect_err("keg conflict");
    assert_eq!(
        error.to_string(),
        format!(
            "Could not symlink bin/tool\nTarget {}/bin/tool is a symlink belonging to bar. You can unlink it:\n  brew unlink bar\nTo force the link and overwrite all conflicting files:\n  brew link --overwrite foo\n\nTo list all files that would be deleted:\n  brew link --overwrite foo --dry-run",
            ctx.env.prefix
        )
    );
}

#[tokio::test]
async fn dry_run_lists_exact_paths_and_is_byte_identical_for_link_and_overwrite() {
    let fixture = Fixture::new();
    let foo = fixture.keg("foo", "1.0", 0);
    fixture.keg_file(&foo, "bin/tool", "foo");
    let (ctx, reporter) = fixture.context(vec![formula("foo", "1.0", 0)]);
    fs::create_dir_all(&ctx.env.prefix).expect("prefix");
    write(&ctx.env.locks.join("foo.formula.lock"), "");
    let before = fingerprint(&ctx.env.prefix);

    link::run(
        &ctx,
        Args {
            names: vec!["foo".to_owned()],
            dry_run: true,
            ..Args::default()
        },
    )
    .await
    .expect("link dry run");
    assert_eq!(fingerprint(&ctx.env.prefix), before);
    assert_eq!(
        reporter.take(),
        [
            "print:Would link:".to_owned(),
            format!("print:{}", ctx.env.prefix.join("bin/tool")),
        ]
    );

    write(&ctx.env.prefix.join("bin/tool"), "user");
    let before = fingerprint(&ctx.env.prefix);
    link::run(
        &ctx,
        Args {
            names: vec!["foo".to_owned()],
            overwrite: true,
            dry_run: true,
            force: false,
        },
    )
    .await
    .expect("overwrite dry run");
    assert_eq!(fingerprint(&ctx.env.prefix), before);
    assert_eq!(
        reporter.take(),
        [
            "print:Would remove:".to_owned(),
            format!("print:{}", ctx.env.prefix.join("bin/tool")),
        ]
    );
}

#[tokio::test]
async fn keg_only_force_versioned_macos_and_path_messages_are_exact() {
    let fixture = Fixture::new();
    let forced = fixture.keg("forced", "1.0", 0);
    let versioned = fixture.keg("versioned@1", "1.0", 0);
    let macos = fixture.keg("macos", "1.0", 0);
    fixture.keg_file(&forced, "bin/forced", "forced");
    fixture.keg_file(&versioned, "bin/versioned", "versioned");
    fixture.keg_file(&macos, "bin/macos", "macos");
    let formulae = vec![
        keg_only_formula("forced", "1.0", 0, "some_reason", "reason"),
        keg_only_formula("versioned@1", "1.0", 0, ":versioned_formula", "versioned"),
        keg_only_formula(
            "macos",
            "1.0",
            0,
            "provided_by_macos",
            "macOS already provides it.",
        ),
    ];
    let (ctx, reporter) = fixture.context(formulae);

    link::run(
        &ctx,
        Args {
            names: vec!["forced".to_owned()],
            ..Args::default()
        },
    )
    .await
    .expect("force warning");
    assert_eq!(
        reporter.take(),
        [
            "opoo:forced is keg-only and must be linked with `--force`.".to_owned(),
            format!(
                "print:\nIf you need to have this software first in your PATH instead consider running:\n  echo 'export PATH=\"{}/opt/forced/bin:$PATH\"' >> {}",
                ctx.env.prefix,
                ctx.env.home.join(".profile")
            ),
        ]
    );
    assert!(!is_symlink(&ctx.env.linked.join("forced")));

    link::run(
        &ctx,
        Args {
            names: vec!["forced".to_owned()],
            force: true,
            ..Args::default()
        },
    )
    .await
    .expect("forced link");
    assert!(is_symlink(&ctx.env.prefix.join("bin/forced")));
    assert!(reporter.take()[0].contains("3 symlinks created."));

    link::run(
        &ctx,
        Args {
            names: vec!["versioned@1".to_owned()],
            ..Args::default()
        },
    )
    .await
    .expect("versioned links without force");
    assert!(is_symlink(&ctx.env.prefix.join("bin/versioned")));
    assert_eq!(
        reporter.take().len(),
        1,
        "versioned formula prints no PATH hint"
    );

    let mut mac_ctx = ctx;
    mac_ctx.env.bottle_tag =
        zapbrew_types::BottleTag::from_host("macos", "arm64", Some("tahoe")).expect("mac tag");
    link::run(
        &mac_ctx,
        Args {
            names: vec!["macos".to_owned()],
            force: true,
            ..Args::default()
        },
    )
    .await
    .expect("custom macOS prefix allows forced link");
    assert!(reporter.take()[0].contains("3 symlinks created."));
    assert!(is_symlink(&fixture.env.linked.join("macos")));
}

#[tokio::test]
async fn linking_unlinks_locked_keg_only_versioned_sibling_first() {
    let fixture = Fixture::new();
    let target = fixture.keg("foo", "2.0", 0);
    let sibling = fixture.keg("foo@1", "1.0", 0);
    fixture.keg_file(&target, "bin/foo", "target");
    fixture.keg_file(&sibling, "bin/foo", "sibling");
    pour_link(
        &sibling,
        &Prefix::new(fixture.env.clone()),
        LinkOptions {
            force: true,
            keg_only: true,
            ..LinkOptions::default()
        },
    )
    .expect("sibling link");
    let (ctx, reporter) = fixture.context(vec![
        formula("foo", "2.0", 0),
        keg_only_formula("foo@1", "1.0", 0, ":versioned_formula", "versioned"),
    ]);

    link::run(
        &ctx,
        Args {
            names: vec!["foo".to_owned()],
            ..Args::default()
        },
    )
    .await
    .expect("replace sibling links");

    assert_eq!(
        reporter.take(),
        [
            format!("print:Unlinking {}... 1 symlinks removed.", sibling.path()),
            format!("print:Linking {}... 3 symlinks created.", target.path()),
        ]
    );
    assert!(is_symlink(&ctx.env.linked.join("foo")));
    assert!(!is_symlink(&ctx.env.linked.join("foo@1")));
    assert_eq!(
        fs::read_to_string(target.path().join("bin/foo")).expect("target contents"),
        "target"
    );
}

#[tokio::test]
async fn family_unlinks_cover_full_variants_and_every_unlinked_sibling_was_locked() {
    let fixture = Fixture::new();
    let target = fixture.keg("foo", "3.0", 0);
    let versioned = fixture.keg("foo@2", "2.0", 0);
    let full = fixture.keg("foo-full", "1.0", 0);
    let versioned_full = fixture.keg("foo@2-full", "2.0", 0);
    let unrelated = fixture.keg("food", "1.0", 0);
    fixture.keg_file(&target, "bin/foo", "target");
    fixture.keg_file(&versioned, "bin/foo-versioned", "versioned");
    fixture.keg_file(&full, "bin/foo-full-bin", "full");
    fixture.keg_file(&versioned_full, "bin/foo-versioned-full", "versioned-full");
    fixture.keg_file(&unrelated, "bin/food", "food");
    let prefix = Prefix::new(fixture.env.clone());
    for (keg, keg_only) in [
        (&versioned, true),
        (&full, true),
        (&versioned_full, true),
        (&unrelated, false),
    ] {
        pour_link(
            keg,
            &prefix,
            LinkOptions {
                force: true,
                keg_only,
                ..LinkOptions::default()
            },
        )
        .expect("sibling link");
    }

    let formulae = vec![
        formula("foo", "3.0", 0),
        keg_only_formula("foo@2", "2.0", 0, ":versioned_formula", "versioned"),
        keg_only_formula("foo-full", "1.0", 0, "some_reason", "full"),
        keg_only_formula(
            "foo@2-full",
            "2.0",
            0,
            ":versioned_formula",
            "full versioned",
        ),
        formula("food", "1.0", 0),
    ];
    let (ctx, reporter) = fixture.context(formulae);

    for sibling in ["foo@2", "foo-full", "foo@2-full"] {
        let held =
            zapbrew_prefix::LockGuard::acquire(&ctx.env.locks, &format!("{sibling}.formula.lock"))
                .expect("hold family lock");
        let error = link::run(
            &ctx,
            Args {
                names: vec!["foo".to_owned()],
                ..Args::default()
            },
        )
        .await
        .expect_err("family member must be locked before unlink");
        assert!(
            error
                .to_string()
                .contains(&format!("{sibling}.formula.lock")),
            "expected busy lock for {sibling}, got {error}"
        );
        drop(held);
        let _ = reporter.take();
    }

    link::run(
        &ctx,
        Args {
            names: vec!["foo".to_owned()],
            ..Args::default()
        },
    )
    .await
    .expect("replace family links");

    assert_eq!(
        reporter.take(),
        [
            format!("print:Unlinking {}... 1 symlinks removed.", full.path()),
            format!(
                "print:Unlinking {}... 1 symlinks removed.",
                versioned.path()
            ),
            format!(
                "print:Unlinking {}... 1 symlinks removed.",
                versioned_full.path()
            ),
            format!("print:Linking {}... 3 symlinks created.", target.path()),
        ]
    );
    assert!(is_symlink(&ctx.env.linked.join("foo")));
    assert!(!is_symlink(&ctx.env.linked.join("foo@2")));
    assert!(!is_symlink(&ctx.env.linked.join("foo-full")));
    assert!(!is_symlink(&ctx.env.linked.join("foo@2-full")));
    assert!(
        is_symlink(&ctx.env.linked.join("food")),
        "unrelated prefix remains linked"
    );
    assert_eq!(
        fs::read_to_string(ctx.env.prefix.join("bin/foo")).expect("linked target"),
        "target"
    );
    assert_eq!(
        fs::read_to_string(ctx.env.prefix.join("bin/food")).expect("unrelated"),
        "food"
    );
}

fn formula_with_aliases(
    name: &str,
    version: &str,
    scheme: u32,
    aliases: &[&str],
    oldnames: &[&str],
) -> serde_json::Value {
    let mut value = formula(name, version, scheme);
    value["aliases"] = json!(aliases);
    value["oldnames"] = json!(oldnames);
    value
}

#[tokio::test]
async fn link_creates_alias_and_oldname_opt_and_linked_symlinks() {
    let fixture = Fixture::new();
    let keg = fixture.keg("wget", "1.25.0", 1);
    fixture.keg_file(&keg, "bin/wget", "data");
    let (ctx, _reporter) = fixture.context(vec![formula_with_aliases(
        "wget",
        "1.25.0",
        1,
        &["wgot"],
        &["wget2"],
    )]);

    link::run(
        &ctx,
        Args {
            names: vec!["wget".to_owned()],
            ..Args::default()
        },
    )
    .await
    .expect("link");

    let opt_alias = ctx.env.prefix.join("opt/wgot");
    let opt_old = ctx.env.prefix.join("opt/wget2");
    assert!(is_symlink(&opt_alias), "opt alias symlink created");
    assert!(is_symlink(&opt_old), "opt oldname symlink created");
    assert_eq!(
        fs::read_link(&opt_alias)
            .expect("read alias target")
            .to_string_lossy(),
        "wget"
    );
    assert_eq!(
        fs::read_link(&opt_old)
            .expect("read oldname target")
            .to_string_lossy(),
        "wget"
    );
    assert!(is_symlink(&ctx.env.linked.join("wgot")), "linked alias");
    assert!(is_symlink(&ctx.env.linked.join("wget2")), "linked oldname");
}

#[tokio::test]
async fn unlink_removes_alias_and_oldname_opt_and_linked_symlinks() {
    let fixture = Fixture::new();
    let keg = fixture.keg("wget", "1.25.0", 1);
    fixture.keg_file(&keg, "bin/wget", "data");
    let (ctx, _reporter) = fixture.context(vec![formula_with_aliases(
        "wget",
        "1.25.0",
        1,
        &["wgot"],
        &["wget2"],
    )]);

    link::run(
        &ctx,
        Args {
            names: vec!["wget".to_owned()],
            ..Args::default()
        },
    )
    .await
    .expect("link");

    assert!(is_symlink(&ctx.env.prefix.join("opt/wgot")));
    assert!(is_symlink(&ctx.env.prefix.join("opt/wget2")));

    unlink::run(
        &ctx,
        unlink::Args {
            names: vec!["wget".to_owned()],
            dry_run: false,
        },
    )
    .await
    .expect("unlink");

    assert!(
        !is_symlink(&ctx.env.prefix.join("opt/wgot")),
        "opt alias removed"
    );
    assert!(
        !is_symlink(&ctx.env.prefix.join("opt/wget2")),
        "opt oldname removed"
    );
    assert!(
        !is_symlink(&ctx.env.linked.join("wgot")),
        "linked alias removed"
    );
    assert!(
        !is_symlink(&ctx.env.linked.join("wget2")),
        "linked oldname removed"
    );
    assert!(
        is_symlink(&ctx.env.prefix.join("opt/wget")),
        "opt record retained until uninstall"
    );
}

#[tokio::test]
async fn link_overwrites_stale_alias_symlink_pointing_elsewhere() {
    let fixture = Fixture::new();
    let keg = fixture.keg("wget", "1.25.0", 1);
    fixture.keg_file(&keg, "bin/wget", "data");
    // Plant a stale opt/<alias> symlink pointing at a different formula.
    fs::create_dir_all(fixture.env.prefix.join("opt")).expect("opt dir");
    symlink("other", fixture.env.prefix.join("opt/wgot")).expect("stale alias symlink");

    let (ctx, _reporter) = fixture.context(vec![formula_with_aliases(
        "wget",
        "1.25.0",
        1,
        &["wgot"],
        &["wget2"],
    )]);

    link::run(
        &ctx,
        Args {
            names: vec!["wget".to_owned()],
            ..Args::default()
        },
    )
    .await
    .expect("link");

    let opt_alias = ctx.env.prefix.join("opt/wgot");
    assert!(is_symlink(&opt_alias), "alias symlink recreated");
    assert_eq!(
        fs::read_link(&opt_alias)
            .expect("read alias target")
            .to_string_lossy(),
        "wget",
        "stale alias overwritten to point at the linked formula"
    );
}
