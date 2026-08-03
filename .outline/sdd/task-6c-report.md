# Task 6c report — formula lifecycle operations

## Result

Implemented `uninstall`, `upgrade`, `outdated`, and the uninstall-to-`autoremove` handoff on base `74aa51b373cd0862c7a591689f6cfcb0d98db80d`.

Public entry points:

- `uninstall::run(&Ctx, uninstall::Args { names, force, ignore_dependencies })`
- `upgrade::run(&Ctx, upgrade::Args { names, dry_run })`
- `outdated::run(&Ctx, outdated::Args { names, verbose, json_v2 })`
- `autoremove::run(&Ctx, autoremove::Args { dry_run })`

The only reused implementation surfaces added to the audited install foundation are crate-private bottle/replacement helpers and crate-private transaction operations. No lower crate, CLI, reference, or unrelated verb changed.

## State-machine proof

### Uninstall

1. Parse candidate canonical names without reading receipts.
2. Acquire the complete, sorted `BTreeSet` of formula locks.
3. Scan installed state, then preflight existence, installed dependents, and pins before any mutation.
4. Snapshot opt, linked, and pin records before mutation.
5. Journal the relink inverse before unlinking a target keg.
6. Atomically rename each target keg from the Cellar into an exclusively-created sibling Cellar trash area on the same filesystem.
7. After the last keg is staged, remove the now-empty rack and its records.
8. Commit only after every requested formula is logically removed.
9. Delete staged trash after commit. A failed deletion returns `CleanupIncomplete` with surviving trash paths and does not restore the removed formula.
10. On any pre-commit failure, recreate removed racks, rename staged kegs back in reverse order, relink old kegs, and restore exact record symlink targets in reverse order. Rollback path creation is confined to the configured prefix.
11. Drop uninstall locks before calling `autoremove`; suppression honors `HOMEBREW_NO_AUTOREMOVE`.

The pre-commit injection test proves the original keg, linked record, opt record, and prefix link all resolve to the restored old keg. The post-commit cleanup injection test proves the rack and records remain removed while the error names retained trash.

### Autoremove

1. Enumerate rack names without reading receipts.
2. Acquire the complete sorted lock set in real mode; dry-run remains filesystem read-only.
3. Scan state, then strictly reread and deserialize every `INSTALL_RECEIPT.json`; missing or corrupt receipts are typed errors.
4. Repeatedly add an unrequested formula whose installed dependents are already in the removal set until no candidate remains.
5. Dry-run prints the sorted complete fixpoint and returns without download or mutation.
6. Real mode sends the complete fixpoint through the same locked rename-and-rollback removal transaction, without recursively invoking autoremove.

The three-level `root -> middle -> leaf` test proves multiple fixpoint rounds remove the complete set.

### Outdated

Outdated is read-only and takes no locks. Its shared crate-private predicate is exactly:

`(latest.version_scheme > installed_tab.version_scheme && latest.pkg_version != installed.pkg_version) || latest.pkg_version > max(installed pkg_version)`

`PkgVersion` comparisons remain semantic. Output stays sorted and supports default, verbose pinned, and pretty JSON v2 forms; unpinned `pinned_version` is explicitly `null`.

### Upgrade

1. Resolve named candidates or enumerate installed rack names without reading receipts.
2. Acquire the complete sorted lock set before state scan in real mode; dry-run performs no lock-file, download, or install mutation.
3. Refuse explicitly named pins; warn and skip pins in all mode.
4. Filter through the shared outdated predicate.
5. Build all bottle requests and fetch selected bottles concurrently.
6. For each formula, reuse the audited install transaction with the old `installed_on_request` and a linked-old/no-target replacement journal.
7. The old keg stays present until the new keg is committed and linked. Any pre-commit failure relinks the old keg and removes staging/new state.
8. Unless `HOMEBREW_NO_INSTALL_CLEANUP` is set, atomically rename old kegs to an exclusive confined sibling trash area after commit, then delete them. Cleanup failures warn while the new keg remains active.

The injected post-unlink failure test proves the old keg and linked record are restored and the new keg is absent. The cleanup injection test proves the new keg remains linked after old-keg cleanup fails.

## Tests and exact gate output

New lifecycle integration tests: **23 passed**:

- `tests/uninstall.rs`: 9
- `tests/autoremove.rs`: 4
- `tests/outdated.rs`: 4
- `tests/upgrade.rs`: 6

Final required gate, run after the last source edit:

```text
$ cargo fmt -p zapbrew-ops
exit 0

$ cargo test -p zapbrew-ops
88 passed; 0 failed; 0 ignored; 16 suites

$ cargo clippy -p zapbrew-ops --all-targets -- -D warnings
exit 0; 0 warnings
```

Tests use scratch prefixes, real receipt/link filesystem behavior, real tiny tar.gz bottles, wiremock HTTP, recording reporters, and a command runner that panics if a host command is attempted.

## Self-review

- Confirmed candidate lock sets are sorted and acquired before state scans for real mutations.
- Confirmed dry-run paths perform no bottle fetch or formula filesystem mutation.
- Confirmed all rollback inverses are recorded before their forward mutation.
- Confirmed recursive deletion occurs only after atomic rename and logical commit, with path confinement and symlink-type checks.
- Confirmed retained cleanup trash is outside Cellar enumeration while remaining on the Cellar filesystem.
- Confirmed uninstall is catalog-independent and outdated handles installed formulae missing from the API by warning and skipping.
- Confirmed no stdout/stderr writes, shell execution, unsafe code, `unwrap`, TODO, or lower-crate/API widening was introduced.
