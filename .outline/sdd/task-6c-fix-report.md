# Task 6c audit-fix report

Base: `3717f2d1fcd19d5be37ddd37f480a65cfc66f03f`

## Findings closed

1. **Autoremove lock confinement** — `crates/zapbrew-ops/src/autoremove.rs` now scans only the names enumerated before lock acquisition with `scan_selected`. Unit regression `autoremove::tests::late_rack_is_excluded_from_locked_snapshot` creates a rack after enumeration and proves it survives. Replacing the selected scan with a full scan made the regression fail because `late/1.0` was removed.
2. **Per-keg outdated predicate** — `crates/zapbrew-ops/src/outdated.rs` applies the version-scheme/package-version predicate to each keg and requires every installed keg to be outdated. `crates/zapbrew-ops/tests/outdated.rs` covers mixed old/current versions, mixed schemes, revisions, and an empty rack. `crates/zapbrew-ops/tests/upgrade.rs` proves upgrade uses the same predicate. Mutating `all` to `any` made both lifecycle regressions fail.
3. **Observed rollback link mode** — `crates/zapbrew-ops/src/transaction.rs` records whether each successful unlink removed any public prefix links, for both uninstall and replacement journals, and re-links with `keg_only: true` when the observed surface was empty. The uninstall and upgrade failure regressions assert that rollback restores linked records without creating a prefix file. Forcing normal re-linking made both assertions fail.
4. **Configuration warning text** — `crates/zapbrew-ops/src/uninstall.rs` emits exactly `If desired, remove them manually with rm -rf:`. `reports_leftover_configuration_paths_exactly` snapshots the reporter channel and full body. Restoring the backticks made the snapshot fail.
5. **Autoremove dry-run noun** — dry-run output always uses `formulae`. `single_candidate_dry_run_uses_fixed_formulae_noun` snapshots the one-candidate header. Restoring singular `formula` made the snapshot fail.

## Exact gate

Command:

```text
cargo fmt -p zapbrew-ops && cargo test -p zapbrew-ops && cargo clippy -p zapbrew-ops --all-targets -- -D warnings
```

Result: exit 0.

```text
cargo fmt -p zapbrew-ops: no output
cargo test -p zapbrew-ops: 92 passed, 0 failed, 0 ignored, across 16 suites (including doc-tests)
cargo clippy -p zapbrew-ops --all-targets -- -D warnings: finished successfully with no warnings
```
