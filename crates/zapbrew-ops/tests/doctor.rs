mod support;

use std::collections::BTreeSet;
use std::fs;
use std::os::unix::fs::symlink;

use zapbrew_ops::doctor_test_support;

use support::{Fixture, fingerprint, formula, keg_only_formula, write};

#[test]
fn clean_report_is_exact_and_checker_does_not_mutate_or_run_commands() {
    let fixture = Fixture::new();
    fs::create_dir_all(&fixture.env.cache).expect("cache");
    fs::create_dir_all(fixture.env.prefix.join("bin")).expect("bin");
    let before = fingerprint(fixture.env.prefix.parent().expect("scratch root"));
    let (ctx, reporter) = fixture.context(Vec::new());

    doctor_test_support::run_with(&ctx, &[ctx.env.prefix.join("bin")], &BTreeSet::new())
        .expect("doctor");

    assert_eq!(reporter.take(), ["print:Your system is ready to brew."]);
    assert_eq!(
        before,
        fingerprint(ctx.env.prefix.parent().expect("scratch root"))
    );
}

#[test]
fn reports_sorted_broken_symlinks_and_incomplete_downloads() {
    let fixture = Fixture::new();
    fs::create_dir_all(fixture.env.prefix.join("bin/nested")).expect("prefix bin");
    symlink("missing-z", fixture.env.prefix.join("bin/z")).expect("broken z");
    symlink("missing-a", fixture.env.prefix.join("bin/nested/a")).expect("broken a");
    write(&fixture.env.cache.join("z.incomplete"), "z");
    write(&fixture.env.cache.join("nested/a.incomplete"), "a");
    let (ctx, _reporter) = fixture.context(Vec::new());

    let findings =
        doctor_test_support::findings(&ctx, &[ctx.env.prefix.join("bin")], &BTreeSet::new())
            .expect("findings");

    let broken = findings
        .iter()
        .find(|finding| finding.starts_with("Broken symlinks were found:"))
        .expect("broken finding");
    assert_eq!(
        broken,
        &format!(
            "Broken symlinks were found:\n  {}/bin/nested/a\n  {}/bin/z\nRemove them with `brew cleanup`.",
            ctx.env.prefix, ctx.env.prefix
        )
    );
    let incomplete = findings
        .iter()
        .find(|finding| finding.starts_with("Stray incomplete downloads were found:"))
        .expect("incomplete finding");
    assert_eq!(
        incomplete,
        &format!(
            "Stray incomplete downloads were found:\n  {}/nested/a.incomplete\n  {}/z.incomplete\nRemove them with `brew cleanup`.",
            ctx.env.cache, ctx.env.cache
        )
    );
}

#[test]
fn unlinked_check_excludes_keg_only_and_includes_catalog_missing_racks() {
    let fixture = Fixture::new();
    fixture.keg("normal", "1.0", 0);
    fixture.keg("kegonly", "1.0", 0);
    fixture.keg("catalog-missing", "1.0", 0);
    fs::create_dir_all(&fixture.env.cache).expect("cache");
    let (ctx, _reporter) = fixture.context(vec![
        formula("normal", "1.0", 0),
        keg_only_formula("kegonly", "1.0", 0, "versioned_formula", "test"),
    ]);

    let findings =
        doctor_test_support::findings(&ctx, &[ctx.env.prefix.join("bin")], &BTreeSet::new())
            .expect("findings");
    let unlinked = findings
        .iter()
        .find(|finding| finding.starts_with("You have unlinked kegs"))
        .expect("unlinked finding");
    assert!(unlinked.contains(&format!("  {}/catalog-missing", ctx.env.cellar)));
    assert!(unlinked.contains(&format!("  {}/normal", ctx.env.cellar)));
    assert!(!unlinked.contains("kegonly"));
}

#[test]
fn path_collision_is_reported_only_for_overlapping_tool_names() {
    let fixture = Fixture::new();
    let system_bin = fixture.env.home.join("system-bin");
    write(&fixture.env.prefix.join("bin/shared"), "brew");
    write(&fixture.env.prefix.join("bin/only-brew"), "brew");
    write(&system_bin.join("shared"), "system");
    write(&system_bin.join("only-system"), "system");
    fs::create_dir_all(&fixture.env.cache).expect("cache");
    let (ctx, _reporter) = fixture.context(Vec::new());

    let findings = doctor_test_support::findings(
        &ctx,
        &[system_bin.clone(), ctx.env.prefix.join("bin")],
        &BTreeSet::new(),
    )
    .expect("collision finding");
    let collision = findings
        .iter()
        .find(|finding| finding.starts_with(system_bin.as_str()))
        .expect("collision");
    assert_eq!(
        collision,
        &format!(
            "{system_bin} occurs before {}/bin in your PATH.\nThis means that system-provided programs will be used instead of those\nprovided by Homebrew.\n\nThe following tools exist at both paths:\n  shared",
            ctx.env.prefix
        )
    );

    fs::remove_file(system_bin.join("shared")).expect("remove conflict");
    let findings = doctor_test_support::findings(
        &ctx,
        &[system_bin, ctx.env.prefix.join("bin")],
        &BTreeSet::new(),
    )
    .expect("no collision");
    assert!(
        !findings
            .iter()
            .any(|finding| finding.contains("occurs before"))
    );
}

#[test]
fn missing_bin_and_nonempty_sbin_are_reported_but_empty_sbin_is_silent() {
    let fixture = Fixture::new();
    fs::create_dir_all(&fixture.env.cache).expect("cache");
    fs::create_dir_all(fixture.env.prefix.join("sbin")).expect("sbin");
    let (ctx, _reporter) = fixture.context(Vec::new());

    let findings = doctor_test_support::findings(&ctx, &[], &BTreeSet::new()).expect("findings");
    assert!(
        findings
            .iter()
            .any(|finding| finding == "Homebrew's \"bin\" was not found in your PATH.")
    );
    assert!(!findings.iter().any(|finding| finding.contains("\"sbin\"")));

    write(&ctx.env.prefix.join("sbin/tool"), "tool");
    let findings =
        doctor_test_support::findings(&ctx, &[], &BTreeSet::new()).expect("sbin finding");
    assert!(findings.iter().any(|finding| finding == &format!(
        "Homebrew's \"sbin\" was not found in your PATH but you have installed\nformulae that put executables in {}/sbin.",
        ctx.env.prefix
    )));
}

#[test]
fn injected_writability_reports_sorted_prefix_and_cache_paths() {
    let fixture = Fixture::new();
    fs::create_dir_all(&fixture.env.cache).expect("cache");
    fs::create_dir_all(fixture.env.prefix.join("bin")).expect("bin");
    let (ctx, reporter) = fixture.context(Vec::new());
    let unwritable = BTreeSet::from([ctx.env.prefix.clone(), ctx.env.cache.clone()]);

    doctor_test_support::run_with(&ctx, &[ctx.env.prefix.join("bin")], &unwritable)
        .expect("doctor");
    assert_eq!(
        reporter.take(),
        [format!(
            "opoo:The following directories are not writable by your user:\n  {}\n  {}\nChange their ownership or grant your user write permission.",
            ctx.env.cache, ctx.env.prefix
        )]
    );
}
