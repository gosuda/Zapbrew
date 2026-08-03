# Task 6c final-audit-fix report

Base: `c7e7823` (`fix(ops): preserve lifecycle invariants`)

## Findings closed

1. **Formula-level outdated OR predicate** — `crates/zapbrew-ops/src/outdated.rs` now implements exactly `scheme_bumped_for_all_installed_kegs || latest_pkg_version > max_installed_pkg_version`. The scheme branch requires every installed keg to have a lower `version_scheme` and a different `pkg_version`. The max-pkg branch is unconditional across lower/equal/higher schemes. Empty keg sets are not outdated. `crates/zapbrew-ops/tests/outdated.rs::scheme_by_max_pkg_matrix_and_mixed_keg_boundaries` covers the full lower/equal/higher scheme × lower/equal/higher max-pkg matrix plus mixed old/current, mixed-scheme-with-current, all-scheme-old, revision, and empty-rack boundaries. Upgrade continues to call the same `is_outdated` helper; `tests/upgrade.rs` scheme-bump and mixed-current cases still pass.

2. **Uninstall lock-complete `scan_selected` snapshot** — `crates/zapbrew-ops/src/uninstall.rs` enumerates installed rack names with `Rack::all` (no Tab reads), unions them with validated requested names, acquires that complete sorted lock set, then scans only `scan_selected(&env, &locked_names)`. Unit regression `uninstall::tests::late_rack_is_excluded_and_scanned_names_are_locked` creates a late dependent rack after enumeration/lock acquisition and proves it is neither locked nor scanned while every scanned name is a subset of the held lock set and uninstall of the dependency still succeeds.

## Adjudicated non-finding

Upgrade replacement was not changed. `linked_replacement` still sets `target: None`; transaction rename logic still applies only to `replacement.target`, so upgrade never renames the old keg before commit. It unlinks the old keg, retains it for rollback/post-commit cleanup, and the existing upgrade failure/cleanup regressions remain green without touching `transaction.rs` or `upgrade.rs`.

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
