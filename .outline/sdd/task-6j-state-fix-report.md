# Task 6j state fix report

Both findings from `task-6j-state-fix-brief.md` are resolved: raw uninstall tokens
are confined before any Caskroom join, and the install-time appdir is persisted in
a typed state sidecar inside the version tree instead of assuming `/Applications`.
The raw JSON receipt is unchanged.

## Files changed

| File | Change |
|------|--------|
| `crates/zapbrew-ops/src/cask/state.rs` | New: typed `InstallState` sidecar (`.zapbrew-install-state.json`), write/read with UTF-8 + shape validation, no default fallback |
| `crates/zapbrew-ops/src/cask/mod.rs` | Register `state` module; add `confined_caskroom_child` token guard |
| `crates/zapbrew-ops/src/cask/uninstall.rs` | Confine raw token in `resolve_installed`; read appdir from state in `remove` |
| `crates/zapbrew-ops/src/cask/transaction.rs` | Force path reads old appdir from state; write state before artifact apply |
| `crates/zapbrew-ops/tests/cask_uninstall.rs` | State sidecar in fixtures; traversal/nested/absolute + symlink token regressions |
| `crates/zapbrew-ops/tests/cask_install.rs` | State sidecar in manual fixture; custom-appdir e2e + changed-appdir force regressions |

## Token confinement

A raw (non-catalog) token is joined to the Caskroom only after it validates as
exactly one nonempty normal relative path component. Catalog-resolved canonical
tokens remain authoritative and bypass this raw-token guard. After the lexical
check, the exact Caskroom child is inspected with no-follow metadata, so a symlink
entry is rejected too.

| Requested token | Classification | Outcome |
|-----------------|----------------|---------|
| `firefox` (catalog hit) | canonical, authoritative | resolved to catalog token |
| `firefox` (raw, dir exists) | single normal component | joined, uninstalled |
| `""` | empty | `Cask '' is unavailable.` |
| `.` | current-dir component | `Cask '.' is unavailable.` |
| `..` | parent-dir component | `Cask '..' is unavailable.` |
| `../evil` | traversal | `Cask '../evil' is unavailable.` (sentinel above Caskroom untouched) |
| `nested/token` | nested / separator | `Cask 'nested/token' is unavailable.` |
| `/etc` | absolute | `Cask '/etc' is unavailable.` |
| `link` -> `Caskroom/real` (symlink) | single component, symlink child | `Cask 'link' is unavailable.` (no-follow metadata; `real` survives) |

No host command runs and no external receipt/path is touched on any rejection.

## Install appdir state

The install-time appdir is written to `.zapbrew-install-state.json` in the staged
version tree before artifact application, so atomic promotion (`rename staging ->
Caskroom/<token>/<version>`) carries it into place. The raw receipt JSON stays
byte-faithful. Uninstall and force-replacement old-plan reconstruction read the
appdir from this state; missing or malformed state is a typed `InvalidState` with
no silent `/Applications` default.

| Transition | Old appdir source | New appdir source | State outcome |
|------------|-------------------|-------------------|---------------|
| Fresh install | — | `args.appdir` (or `/Applications`) | state written in staging, promoted into version tree |
| Install failure after state write | — | — | staging cleanup removes the unpromoted state |
| Uninstall | state sidecar (read) | — | removed with the version tree |
| Force replace, same appdir | state sidecar (read) | `args.appdir` | old plan targets use old appdir; new state overwrites old on promotion |
| Force replace, changed appdir | state sidecar A (read) | appdir B | old plan clears/backs up appdir-A targets; new plan deploys to appdir-B; rollback restores old state/version/targets |
| Missing / malformed state | — | — | typed `InvalidState` refusal, no fallback |

The changed-appdir contract is proved end to end: install to appdir A, force
reinstall to appdir B — A's target is cleared while B receives the new artifact.
The custom-appdir regression installs into a scratch appdir, uninstalls with no
appdir argument, and confirms the custom target and version tree are removed while
`/Applications` is never referenced.

## Verification gate

Command: `cargo fmt --all && cargo test -p zapbrew-ops --test cask_install --test cask_uninstall && cargo clippy -p zapbrew-ops --all-targets -- -D warnings`

```
   Compiling zapbrew-ops v0.1.0 (/home/alpha/rewrite/zapbrew/crates/zapbrew-ops)
    Finished `test` profile [unoptimized + debuginfo] target(s) in 2.69s
     Running tests/cask_install.rs

running 14 tests
test unsupported_artifact_preflights_before_io ... ok
test missing_cask_refuses ... ok
test dmg_extract_detaches_on_ditto_failure ... ok
test old_token_rename_resolves ... ok
test linux_refuses_all_cask_verbs ... ok
test dmg_source_through_staging_symlink_refuses_before_mutation ... ok
test custom_appdir_uninstall_removes_stored_target_without_appdir_arg ... ok
test pkg_rollback_reports_irreversible_leftover ... ok
test force_reinstall_failure_restores_old_artifact_and_version ... ok
test artifact_mappings_cover_appendix_h ... ok
test force_reinstall_replaces_old_artifact_and_clears_backups ... ok
test receipt_written_then_already_installed_noops ... ok
test zip_and_tar_and_bare_extract_place_app ... ok
test force_reinstall_with_changed_appdir_uses_each_plan_own_appdir ... ok

test result: ok. 14 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.15s

     Running tests/cask_uninstall.rs

running 9 tests
test unsafe_remove_path_fails_before_any_directive ... ok
test traversal_and_nested_and_absolute_tokens_refuse_before_join ... ok
test traversal_launchctl_label_refuses_before_any_command ... ok
test uninstall_resolves_old_token_alias ... ok
test symlink_token_refuses_and_preserves_target ... ok
test uninstall_removed_from_catalog_uses_receipt ... ok
test zap_adds_trash_and_rmdir_directives ... ok
test missing_install_and_missing_receipt_refuse ... ok
test uninstall_reverses_artifacts_and_only_uninstall_directives ... ok

test result: ok. 9 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.08s

    Checking zapbrew-ops v0.1.0 (/home/alpha/rewrite/zapbrew/crates/zapbrew-ops)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 2.24s
GATE_EXIT=0
```
