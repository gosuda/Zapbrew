# Task 6a final contract fixes report

## Files

- `crates/zapbrew-ops/src/dependency.rs`
- `crates/zapbrew-ops/src/platform.rs`
- `crates/zapbrew-ops/tests/dependency.rs`
- `crates/zapbrew-ops/tests/platform.rs`
- `.outline/sdd/task-6a-fix2-report.md`

No manifest, lockfile, lower crate, CLI, or other ops module was edited.

## Changes

- Replaced `UsesMode` with typed `UsesOptions`, carrying the concrete `BottleTag` host, recursive selection, and independent build, test, and optional edge selectors.
- Inverse dependency lookup now requires every requested target, canonicalizes aliases through the catalog, follows the same host/since `uses_from_macos` gate as expansion, applies one filtered edge relation to direct and recursive traversal, remains cycle-safe, and returns sorted unique formula names.
- Required and recommended edges are included by default. Build, test, and optional edges require their matching selectors; a combined recommended/optional occurrence retains the expansion rule that recommended outranks optional.
- `hdiutil_attach` now emits exactly `hdiutil attach -plist -nobrowse -readonly -mountrandom <temp_root> <image>`.

## Verification

Exact command:

```text
cargo fmt -p zapbrew-ops && cargo test -p zapbrew-ops --test dependency --test platform && cargo clippy -p zapbrew-ops --all-targets -- -D warnings
```

Exact output:

```text
   Compiling zapbrew-ops v0.1.0 (/home/alpha/.omp/wt/te5c5f1152/m/crates/zapbrew-ops)
    Finished `test` profile [unoptimized + debuginfo] target(s) in 1.19s
     Running tests/dependency.rs (/tmp/cargo_cache/56/2ced8b1ab8bf20/debug/deps/dependency-be24003963205a59)

running 13 tests
test inverse_uses_requires_every_target ... ok
test inverse_uses_reports_a_missing_target ... ok
test missing_formula_is_typed_for_roots_and_edges ... ok
test inverse_uses_rejects_the_universal_host_tag ... ok
test inverse_uses_from_macos_obeys_linux_and_since_bound_hosts ... ok
test recommended_outranks_optional_within_one_tagged_occurrence ... ok
test active_stack_reports_the_closed_cycle ... ok
test recursive_inverse_uses_is_cycle_safe ... ok
test expansion_is_post_order_and_merges_every_occurrence ... ok
test pour_filter_runs_after_duplicate_tag_merge ... ok
test inverse_uses_is_sorted_unique_and_optionally_recursive ... ok
test inverse_uses_filters_edge_classes_until_selected ... ok
test uses_from_macos_obeys_linux_and_since_bound_hosts ... ok

test result: ok. 13 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.01s

     Running tests/platform.rs (/tmp/cargo_cache/56/2ced8b1ab8bf20/debug/deps/platform-ba542131e7c9a4a1)

running 6 tests
test cask_specs_are_argument_safe_and_exact ... ok
test git_specs_match_tap_clone_and_update_contracts ... ok
test run_checked_maps_spawn_failure_to_typed_io ... ok
test run_checked_preserves_nonzero_status_and_stderr_context ... ok
test service_specs_include_platform_scoping ... ok
test run_checked_returns_captured_success_from_injected_runner ... ok

test result: ok. 6 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s

    Checking zapbrew-ops v0.1.0 (/home/alpha/.omp/wt/te5c5f1152/m/crates/zapbrew-ops)
    Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.61s
```

Test count: **19 passed** (13 dependency + 6 platform), with 0 failed, 0 ignored, 0 measured, and 0 filtered out. Clippy completed with `-D warnings` and emitted no warnings.

## Self-review

- Scope review found only the four named ops files and this required report changed.
- API review confirmed `UsesMode` has been removed and no stale inverse-uses callsite remains under `crates/`.
- Dependency review confirmed the same filtered, host-aware edge iterator drives direct and recursive queries; recursive traversal marks canonical formula names before descent, so cycles terminate without weakening reachability.
- Intersection review confirmed candidates are retained only when the reached-name set contains every canonical target; requested targets themselves remain excluded.
- Tag review confirmed required/recommended default inclusion, opt-in build/test/optional dimensions, and recommended-over-optional precedence shared with expansion.
- Error review confirmed missing requested targets remain typed `OpError::MissingFormula` failures and `BottleTag::All` is rejected as a non-host.
- Platform review confirmed argument order and spelling against the required hdiutil command without shell composition.
- No unresolved self-review findings remain.
