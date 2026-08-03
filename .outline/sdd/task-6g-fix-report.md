# Task 6g audit-fix report

Base: `9d130c1` (`feat(ops): maintain prefix health`)

## Findings closed

1. **Cleanup configured-root no-follow** — `cleanup.rs` now inspects `prefix`, `cellar`, `cache`, and `locks` with `symlink_metadata` via `ensure_real_root` / `present_real_directory` before any `exists`, `read_dir`, walk, or mutation. Missing roots remain allowed; symlink/non-directory roots return typed `InvalidState` (`cleanup root is not a real directory: …`) and do not touch sentinel target bytes. Cache/lock/cellar descent helpers use the same real-directory gate instead of follow-aware `exists()`.

2. **Config scp credential redaction** — `contains_credentials` still redacts URL userinfo and credential query markers. New `scp_style_credentials` covers `<userinfo>@host:path` remotes: known transport usernames `git` / `ssh` / `hg` / `svn` are preserved; every other userinfo (including token-like usernames) becomes `ORIGIN: set`. Insta snapshots cover normal SSH, token-scp, ssh transport user, and credentialed HTTPS.

3. **Config Core tap cache-root no-follow** — `core_json_line` inspects `env.cache` itself with no-follow metadata before `cache/api` or `formula.jws.json`. Missing/symlink/non-directory cache yields `Core tap: N/A` and never inspects the symlink target.

4. **Doctor root confinement** — `findings` validates prefix and cache with `symlink_metadata` before broken-link, incomplete, PATH, or writability helpers. Symlink/non-directory roots become one sorted finding naming the configured root(s) and are not traversed; missing roots keep existing missing/writability behavior. Sentinel regressions prove external target paths never appear in findings and sentinel bytes stay intact.

## Redaction table

| Origin form | Example | Result |
|---|---|---|
| Normal SSH scp | `git@github.com:Homebrew/brew.git` | printed |
| Known transport scp | `ssh@example.test:zapbrew/repo.git` | printed |
| Token-like scp userinfo | `ghp_…@github.com:org/private.git` | `set` |
| URL userinfo | `https://user:token@example.test/zapbrew.git` | `set` |
| Credential query | `…?token=…` / sensitive keys | `set` |

## Confinement / redaction proof paths

- `crates/zapbrew-ops/tests/cleanup.rs::symlinked_cache_root_is_rejected_without_mutating_target`
- `crates/zapbrew-ops/tests/cleanup.rs::non_directory_cache_root_is_typed_invalid_state`
- `crates/zapbrew-ops/tests/config.rs::normal_ssh_and_token_scp_origin_snapshots`
- `crates/zapbrew-ops/tests/config.rs::symlinked_cache_root_yields_core_tap_na_without_following`
- `crates/zapbrew-ops/tests/doctor.rs::symlinked_roots_report_configured_paths_without_traversing_sentinels`
- `crates/zapbrew-ops/tests/doctor.rs::non_directory_configured_root_is_reported_without_descent`
- Snapshots: `crates/zapbrew-ops/tests/snapshots/config__{normal_ssh,token_scp,ssh_transport_user,credentialed_https}.snap`

## Exact gate

Command:

```text
cargo fmt -p zapbrew-ops && cargo test -p zapbrew-ops --test cleanup --test doctor --test config && cargo clippy -p zapbrew-ops --all-targets -- -D warnings
```

Result: exit 0.

```text
     Running tests/cleanup.rs

running 15 tests
test age_requires_both_mtime_and_ctime_strictly_before_cutoff ... ok
test candidate_name_set_is_deterministic ... ok
test missing_named_formula_is_typed_and_mutates_nothing ... ok
test symlinked_prefix_parent_is_rejected_without_traversing_target ... ok
test non_directory_cache_root_is_typed_invalid_state ... ok
test valid_alias_protects_target_and_outside_alias_is_removed_without_following ... ok
test failure_is_aggregated_after_other_candidates_are_processed ... ok
test named_alias_scope_and_no_cleanup_formulae_are_honored ... ok
test symlinked_cache_root_is_rejected_without_mutating_target ... ok
test real_cleanup_removes_broken_links_and_owned_empty_directories_then_touches_marker ... ok
test stale_lock_is_candidate_but_busy_lock_stays ... ok
test catalog_latest_missing_warns_and_keeps_every_installed_keg ... ok
test keeps_current_linked_and_pinned_kegs_while_selecting_old_and_old_scheme ... ok
test scrub_keeps_catalog_latest_and_every_installed_version ... ok
test dry_run_lists_sorted_sizes_without_mutating_or_touching_marker ... ok

test result: ok. 15 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.12s

     Running tests/config.rs

running 7 tests
test non_git_and_tool_failures_use_stable_values_and_record_exact_argv ... ok
test credentialed_origin_is_redacted ... ok
test core_json_symlink_and_symlinked_parent_are_not_followed ... ok
test renders_exact_order_redaction_defaults_and_build_rustc ... ok
test custom_cellar_is_printed_immediately_after_prefix ... ok
test symlinked_cache_root_yields_core_tap_na_without_following ... ok
test normal_ssh_and_token_scp_origin_snapshots ... ok

test result: ok. 7 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.22s

     Running tests/doctor.rs

running 8 tests
test missing_bin_and_nonempty_sbin_are_reported_but_empty_sbin_is_silent ... ok
test symlinked_roots_report_configured_paths_without_traversing_sentinels ... ok
test non_directory_configured_root_is_reported_without_descent ... ok
test injected_writability_reports_sorted_prefix_and_cache_paths ... ok
test clean_report_is_exact_and_checker_does_not_mutate_or_run_commands ... ok
test path_collision_is_reported_only_for_overlapping_tool_names ... ok
test reports_sorted_broken_symlinks_and_incomplete_downloads ... ok
test unlinked_check_excludes_keg_only_and_includes_catalog_missing_racks ... ok

test result: ok. 8 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.06s

cargo clippy -p zapbrew-ops --all-targets -- -D warnings
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 12.53s
```
