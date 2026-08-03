# Task 6b audit fix report

## Result

DONE. The install transaction now owns staging exclusively, crosses an explicit committed boundary before destructive cleanup, and executes preflighted structured post-install actions with rollback-journaled filesystem mutations and injected argv-only commands.

## Supported and rejected structured steps

| Serialized type | Status | Implemented contract |
|---|---|---|
| `mkdir` | Supported | Single-directory creation; journal removes created directory on rollback. |
| `mkdir_p` | Supported | Parent creation in order; every created directory is journaled. |
| `touch` | Supported | Creates missing parents/file; existing files are preserved. |
| `move` | Supported | `overwrite`, `force`, and single-match `source_glob`; destination backup and inverse move are journaled. |
| `move_children`, `move_contents` | Supported | Ordered child moves into a created target with inverse moves. |
| `copy` | Supported | File, symlink, and recursive directory copy; `overwrite` and single-match `source_glob`; replaced destination journaled. |
| `remove` | Supported | Ordered paths/globs, recursive removal, symlink-target/content filters; removals are rename-aside journal entries. |
| `inreplace` | Supported | Literal replacement with `first_only` or global semantics and audit-on-no-match; original is journaled. Regex mode is rejected during preflight because this crate has no regex dependency and the brief forbids manifest changes. |
| `link_dir` | Supported | Recursive directory mirroring with confined symlinks and replacement journal entries. |
| `link_children` | Supported | Ordered child links with expanded prefix/suffix. |
| `symlink` | Supported | Relative or confined absolute source, `force`, and multi-match `source_glob`; target replacement journaled. |
| `write` | Supported | Template expansion and `overwrite`; original bytes/mode/symlink entry preserved by journal rename. |
| `warn` | Supported | Expanded message through injected `Reporter::opoo`. |
| `set_permissions` | Supported | Octal mode, recursive/non-recursive operation, prior modes journaled. |
| `run` | Supported | Injected `CommandRunner`, argv/env/cwd only, no shell; executable confined to current/dependency keg or opt. `sudo` and stdin/stdout redirection are rejected during preflight. |
| `compile_gsettings_schemas` | Supported | Injected `glib-compile-schemas` maintenance command. |
| `gio_querymodules` | Supported | Injected `gio-querymodules` maintenance command. |
| `gdk_pixbuf_query_loaders` | Supported | Injected `gdk-pixbuf-query-loaders --update-cache`. |
| `gtk_update_icon_cache` | Supported | Injected GTK 4/3 cache command selected from installed/catalog helpers. |
| `update_mime_database` | Supported | Injected `update-mime-database`. |
| `update_desktop_database` | Supported | Injected `update-desktop-database`. |
| `init_data_dir`, `terminate_process`, `change_dylib_id`, `configure_gcc_runtime`, `install_gzipped_executable`, `configure_glibc_runtime`, `configure_clang_system`, `configure_php`, `bootstrap_cpython`, `bootstrap_pypy`, `set_ownership`, `delete_keychain_certificate` | Rejected | Specialized actions are rejected as typed `OpError::InstallStep` before locks, downloads, or mutation. |
| Any unknown type/base/token/guard/field | Rejected | Closed parser returns `OpError::InstallStep { formula, index, step_type, reason }` before mutation. |

Allowlisted bases are current `prefix`/keg, stable `opt`/`opt_prefix`, keg subdirectories (`bin`, `sbin`, `lib`, `libexec`, `share`, `pkgshare`, `include`), `etc`, `pkgetc`, `var`, `homebrew_prefix`, and explicit current/dependency `formula_prefix`, `formula_opt_prefix`, and `formula_pkgetc`. `home`, `temp`, `absolute`, absolute path strings, `..`, escaping symlink ancestors, unknown formula references, and non-dependency formula references are rejected.

## Transaction proofs

- **Exclusive staging:** `create_stage` calls `fs::create_dir`, retries only `AlreadyExists` with PID plus process-global monotonic IDs, and records `stage_root` only after successful exclusive creation. `stale_stage_collision_is_never_adopted_or_rolled_back` forces the first candidate collision, verifies byte-identical sentinel data, and verifies rollback leaves only the stale stage.
- **Committed cleanup:** structured filesystem steps complete before `journal.committed = true`. Old-backup and step-journal deletion happens only afterward. `install` bypasses rollback for every post-commit error. `partial_backup_cleanup_failure_keeps_new_keg_active` injects partial old-backup deletion, receives typed `CleanupIncomplete`, and proves the new keg plus bin/opt/linked links remain active.
- **Structured rollback:** every created/replaced/removed/moved/mode-mutated path receives an inverse before mutation. `command_failure_rolls_back_prior_filesystem_steps_and_new_keg` proves a failing injected command reverses earlier file steps and the enclosing keg/link transaction.
- **Preflight:** all formula plans are parsed before formula locks; unknown/malformed actions, bad paths/bases/tokens/guards, unsupported command fields, and specialized types fail before cache/prefix mutation. Maintenance commands are collected and run last through `CommandRunner` after filesystem actions validate.
- **Legacy hook:** structured actions run regardless of `post_install_defined`; after structured success the exact warning remains: `<name> defines post_install; run brew postinstall <name> to execute it.`

## Self-review

A read-only reviewer found one semantic defect: `opt_prefix` and `formula_opt_prefix` initially resolved to versioned keg paths. The implementation was corrected to resolve stable `<prefix>/opt/<formula>` paths and the exact full gate was rerun afterward. No manifest, lower-crate, CLI, Cargo.lock, or unrelated verb file changed.

## Exact verification output

`cargo fmt -p zapbrew-ops` produced no output and exited 0.

```text
$ cargo test -p zapbrew-ops
   Compiling zapbrew-ops v0.1.0 (/home/alpha/.omp/wt/t7295494e3/m/crates/zapbrew-ops)
    Finished `test` profile [unoptimized + debuginfo] target(s) in 2.47s
     Running unittests src/lib.rs (/tmp/cargo_cache/e0/6e574f10f5be89/debug/deps/zapbrew_ops-181e7fb7885e6d46)

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

     Running tests/context.rs (/tmp/cargo_cache/e0/6e574f10f5be89/debug/deps/context-ab5697bc5103cb26)

running 1 test
test context_owns_all_boundaries_and_reporter_is_object_safe ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.07s

     Running tests/dependency.rs (/tmp/cargo_cache/e0/6e574f10f5be89/debug/deps/dependency-f63d67b3c3e36b23)

running 13 tests
test missing_formula_is_typed_for_roots_and_edges ... ok
test inverse_uses_reports_a_missing_target ... ok
test active_stack_reports_the_closed_cycle ... ok
test recommended_outranks_optional_within_one_tagged_occurrence ... ok
test recursive_inverse_uses_is_cycle_safe ... ok
test expansion_is_post_order_and_merges_every_occurrence ... ok
test inverse_uses_requires_every_target ... ok
test pour_filter_runs_after_duplicate_tag_merge ... ok
test inverse_uses_rejects_the_universal_host_tag ... ok
test inverse_uses_filters_edge_classes_until_selected ... ok
test inverse_uses_is_sorted_unique_and_optionally_recursive ... ok
test uses_from_macos_obeys_linux_and_since_bound_hosts ... ok
test inverse_uses_from_macos_obeys_linux_and_since_bound_hosts ... ok

test result: ok. 13 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.02s

     Running tests/error.rs (/tmp/cargo_cache/e0/6e574f10f5be89/debug/deps/error-c19f4ebeebcf1186)

running 3 tests
test io_and_command_failures_keep_machine_readable_context ... ok
test lower_crate_errors_are_transparent_and_cli_prefix_free ... ok
test operation_variants_have_typed_payloads_and_no_cli_prefix ... ok

test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

     Running tests/fetch.rs (/tmp/cargo_cache/e0/6e574f10f5be89/debug/deps/fetch-11be70d5088f58ad)

running 3 tests
test missing_bottle_errors_without_cache_or_cellar_mutation ... ok
test fresh_then_reused_print_exact_path_and_checksum ... ok
test deps_fetches_postorder_with_deterministic_messages ... ok

test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.08s

     Running tests/install.rs (/tmp/cargo_cache/e0/6e574f10f5be89/debug/deps/install-cdfb86514613856f)

running 8 tests
test dry_run_has_zero_http_and_filesystem_mutation ... ok
test keg_only_keeps_records_without_file_links ... ok
test only_dependencies_skips_root ... ok
test installs_real_bottle_with_tab_links_skeleton_caveats_and_summary ... ok
test installs_dependencies_postorder_and_marks_graph_slice ... ok
test already_current_warns_without_fetch_and_force_replaces ... ok
test disabled_refuses_and_deprecated_warns ... ok
test refuses_ruby_modes_before_catalog_network_or_filesystem ... ok

test result: ok. 8 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.15s

     Running tests/install_steps.rs (/tmp/cargo_cache/e0/6e574f10f5be89/debug/deps/install_steps-215277cea911b7b8)

running 5 tests
test symlink_ancestor_escape_is_rejected_before_outside_mutation ... ok
test command_failure_rolls_back_prior_filesystem_steps_and_new_keg ... ok
test structured_steps_run_without_legacy_hook_then_exact_legacy_warning_is_emitted ... ok
test executes_all_generic_filesystem_steps_guards_tokens_and_commands ... ok
test malformed_steps_fail_before_lock_download_or_mutation ... ok

test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.43s

     Running tests/platform.rs (/tmp/cargo_cache/e0/6e574f10f5be89/debug/deps/platform-6beb79b5b666a69d)

running 6 tests
test git_specs_match_tap_clone_and_update_contracts ... ok
test cask_specs_are_argument_safe_and_exact ... ok
test run_checked_maps_spawn_failure_to_typed_io ... ok
test run_checked_preserves_nonzero_status_and_stderr_context ... ok
test service_specs_include_platform_scoping ... ok
test run_checked_returns_captured_success_from_injected_runner ... ok

test result: ok. 6 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

     Running tests/reinstall.rs (/tmp/cargo_cache/e0/6e574f10f5be89/debug/deps/reinstall-4ab677ec036ca432)

running 3 tests
test reinstall_requires_installed_formula ... ok
test reinstall_preserves_installed_on_request_and_replaces_same_keg ... ok
test link_failure_restores_exact_old_keg_receipt_and_links ... ok

test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.13s

     Running tests/state.rs (/tmp/cargo_cache/e0/6e574f10f5be89/debug/deps/state-53eb76b1208a7649)

running 5 tests
test malformed_keg_directory_surfaces_the_lower_typed_error ... ok
test missing_cellar_scans_as_an_empty_snapshot ... ok
test rejects_symlinked_rack_escape ... ok
test rejects_symlinked_keg_escape ... ok
test scan_records_semantic_order_links_pins_files_size_and_receipt_edges ... ok

test result: ok. 5 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.02s

     Running tests/transaction.rs (/tmp/cargo_cache/e0/6e574f10f5be89/debug/deps/transaction-71dcae8f161f2bfd)

running 6 tests
test locks_are_taken_in_sorted_name_order_and_released_on_error ... ok
test cellar_symlink_is_refused_without_outside_mutation ... ok
test skeleton_copy_failure_removes_prior_copies_links_and_promoted_keg ... ok
test partial_backup_cleanup_failure_keeps_new_keg_active ... ok
test relocation_failure_removes_staging_and_formula_state ... ok
test stale_stage_collision_is_never_adopted_or_rolled_back ... ok

test result: ok. 6 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.11s

   Doc-tests zapbrew_ops

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s
```

```text
$ cargo clippy -p zapbrew-ops --all-targets -- -D warnings
    Checking zapbrew-ops v0.1.0 (/home/alpha/.omp/wt/t7295494e3/m/crates/zapbrew-ops)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 1.36s
```
