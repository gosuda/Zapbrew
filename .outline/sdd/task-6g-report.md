# Task 6g report

## Result

Implemented confined cleanup, pure doctor diagnostics, and deterministic/redacted config output in `zapbrew-ops`.

## Cleanup candidate table

| Candidate | Selected when | Kept when | Apply behavior |
|---|---|---|---|
| Installed keg | Catalog scheme is newer, or equal scheme has a newer package version | Current, linked, pinned, excluded by `HOMEBREW_NO_CLEANUP_FORMULAE`, or catalog latest is not installed | Formula locks acquired in sorted order; each formula uses the audited removal transaction and surviving post-commit trash paths are aggregated |
| Cache incomplete | Basename ends in `.incomplete` | Never | Confined no-follow removal |
| Aged cache entry | Both mtime and ctime are strictly older than `cleanup_max_age_days` | `.cleaned` marker or target/alias in a valid cache-local alias pair | Confined no-follow removal |
| Scrubbed bottle | Bottle version is neither catalog latest nor any installed version | Catalog latest, installed version, out-of-scope named formula, or unrecognized filename | Confined no-follow removal |
| Cache alias | Broken or lexically escapes cache | Valid cache-local target | Alias entry only; target is never followed |
| Lock file | Existing regular file can be locked nonblocking | Busy or non-regular entry | Unlinked only while its guard is held |
| Prefix symlink | Broken symlink below an owned, component-validated prefix link root | Live symlink or any symlinked parent component | Symlink entry only; target is never traversed |
| Prefix directory | Becomes empty after selected broken links/directories are removed | Owned root itself or nonempty directory | Removed bottom-up with `remove_dir` |

Dry-run sorts all paths and prints `Would remove: <path> (<size>)` without creating locks or `.cleaned`. Real runs touch `.cleaned` only after every candidate succeeds. The total uses Homebrew's 1000-based B/KB/MB/GB formatter.

## Installed-state decisions

| State | Decision |
|---|---|
| Current catalog package and scheme | Keep |
| Old and unlinked | Remove |
| Old but linked | Keep and warn |
| Old but pinned | Keep and warn |
| Installed receipt scheme newer than catalog | Keep |
| Catalog latest absent from rack | Keep all and warn |
| Formula excluded by no-cleanup list or alias | Skip |
| Named alias | Resolve to canonical formula before locking/scanning |
| Removal failure | Continue independent candidates and return one `CleanupIncomplete` with actual surviving paths |

## Config keys and redaction

| Order | Row | Source/fallback | Redaction |
|---:|---|---|---|
| 1 | `HOMEBREW_VERSION` | Cargo package version plus Homebrew 5 compatibility text | Fixed format |
| 2 | `ORIGIN` | Injected `git -C <repository> remote get-url origin`; `(none)` | Credentialed URL becomes `set` |
| 3 | `HEAD` | Injected `git -C <repository> rev-parse HEAD`; `(none)` | N/A |
| 4 | `Last commit` | Injected `git -C <repository> log -1 --format=%cd`; `never` | N/A |
| 5 | `Core tap JSON` / `Core tap` | No-follow mtime of component-validated `cache/api/formula.jws.json`; `N/A` | Symlinked file/parent rejected |
| 6 | `HOMEBREW_PREFIX` | Detected Env | N/A |
| 7 | `HOMEBREW_CELLAR` | Only when different from `<prefix>/Cellar` | N/A |
| 8 | Non-default `HOMEBREW_*` | Sorted process environment | Sensitive keys, booleans, credentialed authorities, and credential query parameters become `set` |
| 9 | `Rust` | Exact `$RUSTC --version` captured by `build.rs` | N/A |
| 10 | `CPU` | Available parallelism, pointer width, target arch | N/A |
| 11 | `Git` | Injected `git --version`; `N/A` | N/A |
| 12 | `Curl` | Injected `curl --version`; `N/A` | N/A |

## Doctor checks

The checker accepts parsed PATH entries and a writability probe, performs no mutation or commands, and sorts findings and affected paths. It checks broken prefix symlinks, unlinked non-keg-only racks (including catalog-missing racks), stray incomplete downloads, bin ordering/collisions, nonempty missing sbin, and prefix/cache writability. A clean report is exactly `Your system is ready to brew.`.

## Verification

Exact gate run:

```text
$ cargo fmt -p zapbrew-ops && cargo test -p zapbrew-ops && cargo clippy -p zapbrew-ops --all-targets -- -D warnings
cargo test: 162 passed (30 suites, 0.00s)
OK
```

New maintenance integration tests: 13 cleanup, 6 doctor, and 5 config tests (24 total).

## Self-review

- Confirmed edits stay within Task 6g ops files, Cargo metadata/lock, and this required report.
- Confirmed no `unsafe`, `unwrap`, `TODO`, `println!`, or `eprintln!` in maintenance implementation/build files.
- Confirmed cleanup never recursively follows cache or prefix symlinks, validates multi-component prefix roots, preserves busy locks, and reports transaction leftovers rather than vanished original paths.
- Confirmed doctor uses the pure checker seam and injected writability/PATH tests; the production path reads PATH once and runs no host command.
- Confirmed config uses only injected command execution, stable failure values, exact build compiler capture, deterministic key ordering, component-safe Core JSON metadata, and credential redaction for both environment values and origin URLs.
- Addressed all findings from two independent review passes: origin credential exposure, lost transaction leftovers, Core JSON symlink traversal, credential query parameters, and symlinked prefix-parent traversal.
