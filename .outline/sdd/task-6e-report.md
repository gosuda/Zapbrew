# Task 6e report — dependency queries

## Result

Implemented `deps`, `uses`, and `leaves` over the existing catalog graph and installed-state snapshot. Install and fetch now select `EdgeFilter::ALL` explicitly before the unchanged post-merge pour filter.

## Edge-filter truth table

Let `B`, `T`, `O`, and `R` mean build, test, optional, and recommended tags; `b`, `t`, and `o` are the corresponding include flags; `s` is `skip_recommended`; and `root` is true only for an edge leaving a query root. Query inclusion is:

`(!s || !R) && (required || R || (B && b) || (T && t && root) || (O && o))`, where `required = !(B || T || O || R)`.

The complete query-tag table is:

| B | T | O | R | Included when |
|---:|---:|---:|---:|---|
| 0 | 0 | 0 | 0 | always (required) |
| 0 | 0 | 0 | 1 | `!s` |
| 0 | 0 | 1 | 0 | `o` |
| 0 | 0 | 1 | 1 | `!s` |
| 0 | 1 | 0 | 0 | `t && root` |
| 0 | 1 | 0 | 1 | `!s` |
| 0 | 1 | 1 | 0 | `(t && root) || o` |
| 0 | 1 | 1 | 1 | `!s` |
| 1 | 0 | 0 | 0 | `b` |
| 1 | 0 | 0 | 1 | `!s` |
| 1 | 0 | 1 | 0 | `b || o` |
| 1 | 0 | 1 | 1 | `!s` |
| 1 | 1 | 0 | 0 | `b || (t && root)` |
| 1 | 1 | 0 | 1 | `!s` |
| 1 | 1 | 1 | 0 | `b || (t && root) || o` |
| 1 | 1 | 1 | 1 | `!s` |

Thus inclusion is disjunctive after ignores: a recommended+build edge is included by default, but `skip_recommended` excludes it even with `include_build`. Excluded edges are rejected before catalog lookup and recursion. `EdgeFilter::ALL` enables every class at every depth; install/fetch still apply the existing merged `build || test` pour exclusion afterward.

## Behavior and tests

- `deps`: sorted unique closure, root exclusion, multi-root intersection/union, typed missing formula, one-root tree, declared child order, exact glyphs, repeated shared subtrees, root-only tests, cycle marker, then typed cycle error after output.
- `uses`: host-aware filtered inverse graph, direct/transitive reach, multi-target intersection, scratch-installed restriction, deterministic columns, and silent empty output.
- `leaves`: installed Tab reverse dependencies, tap-prefixed name matching, active-keg intent precedence `opt-linked -> linked -> sole -> latest`, typed filter, sorted output, and silent empty output.
- Graph tests cover required/recommended/build/test/optional, multi-tag disjunction, recommended-ignore precedence, root-only test pruning, excluded missing-edge pruning, and Linux/macOS `uses_from_macos` bounds.

Exact gate run:

```text
cargo fmt -p zapbrew-ops && cargo test -p zapbrew-ops && cargo clippy -p zapbrew-ops --all-targets -- -D warnings
cargo test: 120 passed (23 suites, 0.00s)
OK
```

Focused dependency-query/install/fetch/autoremove run before the full gate: `45 passed (7 suites)`.

## Autoremove consistency

Production `autoremove.rs` was not changed. Existing coverage retains strict receipt errors, multi-round fixpoint removal, exact dry-run headers for one and many (`Would autoremove <n> unneeded formulae:`), and uninstall's `HOMEBREW_NO_AUTOREMOVE` handoff. The added independent consistency test derives leaf rounds from the installed reverse graph (`root`, then `middle`, then `leaf` after prior members are excluded), checks the dry-run set, executes real mode, and proves only that fixpoint is removed while a requested formula and its dependency remain.

## Self-review

No out-of-scope production files changed. The query verbs acquire no locks, mutate no state, perform no network or host commands, and emit only through `Reporter`. No stdout/stderr, unsafe, unwrap, TODO, compatibility shim, or second graph-filter relation was introduced. Install/fetch callsites are the only mutating-verb migrations, both using explicit `EdgeFilter::ALL`; their focused tests and the full ops gate pass.
