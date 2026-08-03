# Task 6a report — zapbrew-ops shared foundation

## Files

- `Cargo.lock`
- `crates/zapbrew-ops/Cargo.toml`
- `crates/zapbrew-ops/src/lib.rs`
- `crates/zapbrew-ops/src/context.rs`
- `crates/zapbrew-ops/src/error.rs`
- `crates/zapbrew-ops/src/state.rs`
- `crates/zapbrew-ops/src/dependency.rs`
- `crates/zapbrew-ops/src/platform.rs`
- `crates/zapbrew-ops/tests/context.rs`
- `crates/zapbrew-ops/tests/error.rs`
- `crates/zapbrew-ops/tests/state.rs`
- `crates/zapbrew-ops/tests/dependency.rs`
- `crates/zapbrew-ops/tests/platform.rs`
- `.outline/sdd/task-6a-report.md`

No lower crate, CLI, plan, or reference file was edited.

## Public APIs

- `Reporter: Send + Sync`: object-safe `ohai`, `oh1`, `opoo`, `onoe`, `print`, and `eprint` methods, each taking `(&self, &str)`.
- `Ctx`: owns `Env`, `reqwest::Client`, `Arc<Catalog>`, `Arc<CaskCatalog>`, `Arc<dyn CommandRunner>`, and `Arc<dyn Reporter>`.
- `OpError`: transparent api/net/pour/prefix wrappers plus typed `Io`, `CommandFailed`, `MissingFormula`, `InvalidState`, `DependencyCycle`, and `Refusal` variants. Display strings omit the CLI-owned `Error: ` prefix.
- `state::scan(&Env) -> Result<InstalledState, OpError>` and immutable `InstalledState`, `InstalledFormula`, and `InstalledKeg` queries. The snapshot records semantic keg ordering, receipts, link/opt/pin state, relative file inventories, regular-file byte totals, and reverse runtime-dependency matches from installed Tabs.
- `dependency::expand`: post-order, active-stack-safe expansion with stable dedup, tag merging, host-aware `uses_from_macos`, and post-merge pour filtering. Public supporting types are `DependencyOptions`, `DependencyMode`, `Necessity`, and `ExpandedDependency`.
- `dependency::uses`: sorted unique direct or recursive inverse matches selected by `UsesMode`.
- `platform::run_checked`: injected runner execution with typed spawn and nonzero-status failures.
- Platform command builders: `git_clone`, `git_pull`, `systemctl`, `launchctl`, `installer`, `pkgutil_forget`, `hdiutil_attach`, `hdiutil_detach`, `ditto`, `unzip`, and `remove_path`, with `SystemctlAction` and `LaunchctlAction` closed action sets.

## Verification

Exact command:

```text
cargo fmt -p zapbrew-ops && cargo test -p zapbrew-ops && cargo clippy -p zapbrew-ops --all-targets -- -D warnings
```

Exact output:

```text
   Compiling zapbrew-ops v0.1.0 (/home/alpha/.omp/wt/tf632372b4/m/crates/zapbrew-ops)
    Finished `test` profile [unoptimized + debuginfo] target(s) in 1.54s
     Running unittests src/lib.rs (/tmp/cargo_cache/15/8dc76b17c61cdc/debug/deps/zapbrew_ops-a413c29d7d3cebee)

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

     Running tests/context.rs (/tmp/cargo_cache/15/8dc76b17c61cdc/debug/deps/context-9231852d48fcefbf)

running 1 test
test context_owns_all_boundaries_and_reporter_is_object_safe ... ok

test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.06s

     Running tests/dependency.rs (/tmp/cargo_cache/15/8dc76b17c61cdc/debug/deps/dependency-be24003963205a59)

running 7 tests
test expansion_is_post_order_and_merges_every_occurrence ... ok
test missing_formula_is_typed_for_roots_and_edges ... ok
test recommended_outranks_optional_within_one_tagged_occurrence ... ok
test pour_filter_runs_after_duplicate_tag_merge ... ok
test active_stack_reports_the_closed_cycle ... ok
test inverse_uses_is_sorted_unique_and_optionally_recursive ... ok
test uses_from_macos_obeys_linux_and_since_bound_hosts ... ok

test result: ok. 7 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s

     Running tests/error.rs (/tmp/cargo_cache/15/8dc76b17c61cdc/debug/deps/error-7d88e214c0b6a5db)

running 3 tests
test io_and_command_failures_keep_machine_readable_context ... ok
test lower_crate_errors_are_transparent_and_cli_prefix_free ... ok
test operation_variants_have_typed_payloads_and_no_cli_prefix ... ok

test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

     Running tests/platform.rs (/tmp/cargo_cache/15/8dc76b17c61cdc/debug/deps/platform-ba542131e7c9a4a1)

running 6 tests
test git_specs_match_tap_clone_and_update_contracts ... ok
test cask_specs_are_argument_safe_and_exact ... ok
test run_checked_maps_spawn_failure_to_typed_io ... ok
test run_checked_returns_captured_success_from_injected_runner ... ok
test run_checked_preserves_nonzero_status_and_stderr_context ... ok
test service_specs_include_platform_scoping ... ok

test result: ok. 6 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

     Running tests/state.rs (/tmp/cargo_cache/15/8dc76b17c61cdc/debug/deps/state-dfcfddac1bae4392)

running 3 tests
test missing_cellar_scans_as_an_empty_snapshot ... ok
test malformed_keg_directory_surfaces_the_lower_typed_error ... ok
test scan_records_semantic_order_links_pins_files_size_and_receipt_edges ... ok

test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.02s

   Doc-tests zapbrew_ops

running 0 tests

test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

    Checking zapbrew-ops v0.1.0 (/home/alpha/.omp/wt/tf632372b4/m/crates/zapbrew-ops)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.79s
```

Test count: **20 passed** (1 context + 7 dependency + 3 error + 6 platform + 3 state), with 0 failed, 0 ignored; unit and doc-test suites contain 0 tests.

## Self-review

- Scope review found only the named ops foundation, lockfile, and this required report changed.
- Boundary review confirmed ops has no stdout/stderr macros, shell invocation, `unsafe`, `unwrap`, or TODO markers. Tests use scratch `Env` paths and recording/panic boundary fakes; none invokes host tools.
- Dependency review covered post-order, active-stack cycles, missing roots/edges, duplicate necessity/build/test merging, pour filtering, Linux/macOS `uses_from_macos`, and direct/recursive inverse queries. Review found that a single occurrence carrying both `recommended` and `optional` was initially classified as optional; the precedence was corrected to recommended and locked by a regression test before the final gate.
- State review confirmed semantic `PkgVersion` order comes from the lower `Rack`/`Keg` APIs, each receipt is loaded once, public access is immutable, and reverse queries read Tab runtime dependencies.
- Platform review confirmed every builder passes owned argv directly to the lower `CommandSpec`, `run_checked` uses only the injected runner, and stderr is retained on nonzero status.
- No unresolved self-review findings remain.
