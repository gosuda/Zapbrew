# Task 6d audit-fix report

- Base: `cbd99f12bbe32841f60c8c5daa32566a8a0c833d`
- Commit: `486ab4cf43934b998410cbb47d1e8589d62750ab` (`fix(ops): report active keg intent`)

## Finding closed

Info status used the latest keg's receipt instead of the active keg's. `render_formula` in `crates/zapbrew-ops/src/info.rs` reported `Installed (on request)` or `Installed (as dependency)` from `InstalledFormula::latest`, which disagrees with Homebrew: `Tab.for_formula` (`.references/brew/Library/Homebrew/tab/tab.rb`) selects the receipt by precedence opt-linked keg, then linked keg, then the sole installed keg, then latest. A newer latest keg installed as a dependency was reported as dependency even when the active opt-linked or linked keg was installed on request.

`intent_keg` now applies exactly that precedence (opt-linked, then linked, then sole, then latest) and its `installed_on_request` drives the status line. The installed-version rows rendered are unchanged: `render_installed` still selects latest plus linked kegs.

## Tests

`crates/zapbrew-ops/tests/info.rs` gained 5 new tests plus 1 updated fixture, all exact inline snapshots and each asserting the read-only prefix fingerprint:

- Updated `installed_revision_link_selection_status_and_multiple_name_separator_are_exact`: older 1.0 is now opt-linked and linked with `installed_on_request = true`, newer latest 2.0_2 has `false`; status flips to `Installed (on request)`; keeps the two-row `[Linked]` versions rendering and the sample plus plain name separator.
- `status_prefers_optlinked_over_linked_when_flags_disagree`: opt-linked 1.0 (true) versus linked 2.0_2 (false) reports `Installed (on request)`, proving opt-linked precedes linked.
- `status_falls_back_to_linked_when_no_optlink`: linked 1.0 (false) versus latest 2.0_2 (true) reports `Installed (as dependency)`, proving linked precedes latest.
- `status_uses_sole_installed_keg`: sole 1.0 (true) reports `Installed (on request)`.
- `status_falls_back_to_latest_when_nothing_linked`: latest 2.0_2 (true) reports `Installed (on request)`.
- `status_uses_optlinked_intent_for_keg_only_formula`: keg-only opt-link with no linked record, 1.0 (true) versus latest 2.0_2 (false) reports `Installed (on request)`.

## Exact gate

Command (from the brief):

```text
cargo fmt -p zapbrew-ops && cargo test -p zapbrew-ops --test info && cargo clippy -p zapbrew-ops --all-targets -- -D warnings
```

Result: exit 0.

```text
$ cargo fmt -p zapbrew-ops -- --check
no output (clean)

$ cargo test -p zapbrew-ops --test info
running 9 tests
test text_without_names_refuses_and_missing_name_is_typed ... ok
test json_v2_preserves_merged_raw_objects_and_catalog_order ... ok
test status_uses_optlinked_intent_for_keg_only_formula ... ok
test full_not_installed_info_is_exact ... ok
test status_falls_back_to_latest_when_nothing_linked ... ok
test status_uses_sole_installed_keg ... ok
test status_falls_back_to_linked_when_no_optlink ... ok
test installed_revision_link_selection_status_and_multiple_name_separator_are_exact ... ok
test status_prefers_optlinked_over_linked_when_flags_disagree ... ok

test result: ok. 9 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.11s

$ cargo clippy -p zapbrew-ops --all-targets -- -D warnings
Finished `dev` profile [unoptimized + debuginfo] target(s) in 0.36s  (no warnings)
```

## Self-review

- Only `crates/zapbrew-ops/src/info.rs` and `crates/zapbrew-ops/tests/info.rs` changed; no other module, manifest, or reference file touched.
- Status selection precedence matches Homebrew `Tab.for_formula` exactly; installed-version row selection is untouched.
- New tests snapshot exact reporter output and prove each precedence step with opposite flags; the read-only invariant is re-asserted per scenario.
- No stubs, no `unsafe`, no `unwrap` in the changed source (tests use `.expect` consistent with the existing file); no network, host commands, or locks added.
- Worktree clean after commit.
