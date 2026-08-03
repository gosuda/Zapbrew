# Task 6b final review fix report

Base: `20bff838713d11b62eb1a4da8b511689823e8f21`

## Finding-by-finding proof

1. **Critical journal ordering**
   - `crates/zapbrew-ops/src/install_steps.rs:1449-1458` records the direct `mkdir` inverse only after `create_dir` succeeds.
   - `crates/zapbrew-ops/src/install_steps.rs:2048-2096` records each `mkdir_p`/parent inverse only after that directory is created.
   - `crates/zapbrew-ops/tests/install_steps.rs:502-541` proves a failed `mkdir` preserves a pre-existing tree's bytes and mode through outer rollback.
2. **Inline run ordering**
   - `crates/zapbrew-ops/src/install_steps.rs:268-292` executes generic `run` steps inline and defers only maintenance commands.
   - `crates/zapbrew-ops/tests/install_steps.rs:544-581` observes FS → run → FS → maintenance state and exact command order.
3. **Prefix, cellar, and keg confinement**
   - `crates/zapbrew-ops/src/install_steps.rs:2374-2426` selects the cellar or prefix root and applies lexical, canonical, and symlink-ancestor confinement; resolver base checks retain the active keg as the strict formula root.
   - `crates/zapbrew-ops/src/install_steps.rs:2824-2848` applies the same root set during rollback without following the final symlink being removed.
   - `crates/zapbrew-ops/tests/install_steps.rs:421-442`, `584-640`, and `696-734` cover prefix escape, external-cellar success, cellar escape, and cross-root link rollback.
4. **Reinstall output parity**
   - `crates/zapbrew-ops/src/install.rs:556-579` exposes caveat substitution and human-size formatting crate-privately.
   - `crates/zapbrew-ops/src/reinstall.rs:98-111` reuses both helpers.
   - `crates/zapbrew-ops/tests/reinstall.rs:264-303` compares install and reinstall caveat/summary output exactly across all supported prefix placeholders and a KB-sized install.
5. **Binding refusals and partial commit**
   - `crates/zapbrew-ops/tests/install.rs:483-627` binds pinned-outdated, installed-conflict text, verified migration hint, missing formula, and missing host bottle refusals.
   - `crates/zapbrew-ops/tests/install_steps.rs:643-693` proves an installed dependency remains committed while a later requested root leaves no rack, keg, links, or staging state.

Mutation checks also reintroduced the old mkdir timing, deferred-run ordering, prefix-only confinement, raw reinstall caveats, and final-symlink rollback checks; each named regression failed before its fix was restored.

## Full gate

Command:

```text
cargo fmt -p zapbrew-ops && cargo test -p zapbrew-ops && cargo clippy -p zapbrew-ops --all-targets -- -D warnings
```

Exact output:

```text
   Compiling zapbrew-ops v0.1.0 (/home/alpha/.omp/wt/te59f41cf1/m/crates/zapbrew-ops)
    Finished `test` profile [unoptimized + debuginfo] target(s) in 2.18s
     Running unittests src/lib.rs (/tmp/cargo_cache/10/e86c13371698fc/debug/deps/zapbrew_ops-181e7fb7885e6d46)

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

     Running tests/context.rs (/tmp/cargo_cache/10/e86c13371698fc/debug/deps/context-ab5697bc5103cb26)

running 1 test
test context_owns_all_boundaries_and_reporter_is_object_safe ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.07s

     Running tests/dependency.rs (/tmp/cargo_cache/10/e86c13371698fc/debug/deps/dependency-f63d67b3c3e36b23)

running 13 tests
test missing_formula_is_typed_for_roots_and_edges ... ok
test inverse_uses_reports_a_missing_target ... ok
test pour_filter_runs_after_duplicate_tag_merge ... ok
test inverse_uses_rejects_the_universal_host_tag ... ok
test recursive_inverse_uses_is_cycle_safe ... ok
test recommended_outranks_optional_within_one_tagged_occurrence ... ok
test inverse_uses_is_sorted_unique_and_optionally_recursive ... ok
test expansion_is_post_order_and_merges_every_occurrence ... ok
test inverse_uses_requires_every_target ... ok
test active_stack_reports_the_closed_cycle ... ok
test inverse_uses_filters_edge_classes_until_selected ... ok
test inverse_uses_from_macos_obeys_linux_and_since_bound_hosts ... ok
test uses_from_macos_obeys_linux_and_since_bound_hosts ... ok

test result: ok. 13 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.02s

     Running tests/error.rs (/tmp/cargo_cache/10/e86c13371698fc/debug/deps/error-c19f4ebeebcf1186)

running 3 tests
test io_and_command_failures_keep_machine_readable_context ... ok
test lower_crate_errors_are_transparent_and_cli_prefix_free ... ok
test operation_variants_have_typed_payloads_and_no_cli_prefix ... ok

test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

     Running tests/fetch.rs (/tmp/cargo_cache/10/e86c13371698fc/debug/deps/fetch-11be70d5088f58ad)

running 3 tests
test fresh_then_reused_print_exact_path_and_checksum ... ok
test missing_bottle_errors_without_cache_or_cellar_mutation ... ok
test deps_fetches_postorder_with_deterministic_messages ... ok

test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.09s

     Running tests/install.rs (/tmp/cargo_cache/10/e86c13371698fc/debug/deps/install-cdfb86514613856f)

running 13 tests
test missing_host_bottle_is_refused_with_host_tag ... ok
test already_current_warns_without_fetch_and_force_replaces ... ok
test keg_only_keeps_records_without_file_links ... ok
test dry_run_has_zero_http_and_filesystem_mutation ... ok
test installs_dependencies_postorder_and_marks_graph_slice ... ok
test only_dependencies_skips_root ... ok
test migrated_formula_refusal_surfaces_verified_tap_hint ... ok
test installs_real_bottle_with_tab_links_skeleton_caveats_and_summary ... ok
test missing_formula_has_exact_typed_message ... ok
test installed_conflict_refusal_has_complete_guidance ... ok
test disabled_refuses_and_deprecated_warns ... ok
test pinned_outdated_formula_is_refused_before_fetch ... ok
test refuses_ruby_modes_before_catalog_network_or_filesystem ... ok

test result: ok. 13 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.19s

     Running tests/install_steps.rs (/tmp/cargo_cache/10/e86c13371698fc/debug/deps/install_steps-215277cea911b7b8)

running 11 tests
test symlink_ancestor_escape_is_rejected_before_outside_mutation ... ok
test executes_all_generic_filesystem_steps_guards_tokens_and_commands ... ok
test keg_steps_support_cellar_outside_prefix ... ok
test rollback_removes_prefix_links_into_external_cellar ... ok
test committed_dependency_survives_later_requested_root_rollback ... ok
test cellar_symlink_ancestor_escape_is_rejected_before_mutation ... ok
test structured_steps_run_without_legacy_hook_then_exact_legacy_warning_is_emitted ... ok
test command_failure_rolls_back_prior_filesystem_steps_and_new_keg ... ok
test failed_mkdir_does_not_journal_or_remove_preexisting_tree ... ok
test run_steps_execute_inline_and_maintenance_runs_last ... ok
test malformed_steps_fail_before_lock_download_or_mutation ... ok

test result: ok. 11 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.48s

     Running tests/platform.rs (/tmp/cargo_cache/10/e86c13371698fc/debug/deps/platform-6beb79b5b666a69d)

running 6 tests
test cask_specs_are_argument_safe_and_exact ... ok
test git_specs_match_tap_clone_and_update_contracts ... ok
test run_checked_maps_spawn_failure_to_typed_io ... ok
test service_specs_include_platform_scoping ... ok
test run_checked_preserves_nonzero_status_and_stderr_context ... ok
test run_checked_returns_captured_success_from_injected_runner ... ok

test result: ok. 6 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

     Running tests/reinstall.rs (/tmp/cargo_cache/10/e86c13371698fc/debug/deps/reinstall-4ab677ec036ca432)

running 4 tests
test reinstall_requires_installed_formula ... ok
test reinstall_caveats_and_summary_match_install_output ... ok
test reinstall_preserves_installed_on_request_and_replaces_same_keg ... ok
test link_failure_restores_exact_old_keg_receipt_and_links ... ok

test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.14s

     Running tests/state.rs (/tmp/cargo_cache/10/e86c13371698fc/debug/deps/state-53eb76b1208a7649)

running 5 tests
test missing_cellar_scans_as_an_empty_snapshot ... ok
test malformed_keg_directory_surfaces_the_lower_typed_error ... ok
test rejects_symlinked_rack_escape ... ok
test rejects_symlinked_keg_escape ... ok
test scan_records_semantic_order_links_pins_files_size_and_receipt_edges ... ok

test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s

     Running tests/transaction.rs (/tmp/cargo_cache/10/e86c13371698fc/debug/deps/transaction-71dcae8f161f2bfd)

running 6 tests
test relocation_failure_removes_staging_and_formula_state ... ok
test locks_are_taken_in_sorted_name_order_and_released_on_error ... ok
test stale_stage_collision_is_never_adopted_or_rolled_back ... ok
test partial_backup_cleanup_failure_keeps_new_keg_active ... ok
test skeleton_copy_failure_removes_prior_copies_links_and_promoted_keg ... ok
test cellar_symlink_is_refused_without_outside_mutation ... ok

test result: ok. 6 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.09s

   Doc-tests zapbrew_ops

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

    Checking zapbrew-ops v0.1.0 (/home/alpha/.omp/wt/te59f41cf1/m/crates/zapbrew-ops)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 1.37s
```

Result: 65 tests passed across 12 test binaries/doc-test targets; clippy completed with `-D warnings` and no diagnostics.
