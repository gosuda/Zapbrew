# Task 6d report — installed formula discovery

## Result

Implemented read-only `list`, `info`, `search`, and `desc` operations on base `84e90a7131aa52c9a9b63809fc1cfb21ea0a62df`. Added deterministic Homebrew-style column rendering and wired the four public modules through `zapbrew-ops`.

No install, lifecycle, lower-crate, CLI, or reference source changed. The only manifest additions are existing workspace dependencies used directly by this slice (`regex`, `serde`, and test-only `insta`), plus the corresponding `Cargo.lock` package dependency update.

## Public API

| Module | Args | Entry point |
|---|---|---|
| `list` | `names`, `versions`, `oneline`, `width` | `pub async fn run(&Ctx, Args) -> Result<(), OpError>` |
| `info` | `names`, `json_v2` | `pub async fn run(&Ctx, Args) -> Result<(), OpError>` |
| `search` | `query`, `desc`, `formula_only`, `cask_only`, `width` | `pub async fn run(&Ctx, Args) -> Result<(), OpError>` |
| `desc` | `names` | `pub async fn run(&Ctx, Args) -> Result<(), OpError>` |

`render::columns` remains crate-private. Width is caller-supplied; width `0` and a computed one-column layout both emit one item per line. Ops does not probe the terminal.

## Output and behavior

| Surface | Implemented behavior |
|---|---|
| Columns | Sorted caller input, gap 2, exact column-major fill, no trailing spaces, newline after every row, empty output for empty input |
| `list` | Sorted canonical installed names; semantic keg-version order; `-1`/width fallback; alias/oldname resolution for named racks; linked-keg preference then latest-keg fallback; sorted absolute non-directory file and symlink paths; exact `No such keg: <cellar>/<name>` refusal |
| Text `info` | Canonical full-name title with stable source version, host-selectable bottle marker, and keg-only marker; description, homepage, aliases, old names, installed status, newest/linked installed keg lines with full `PkgVersion`, GitHub source URL, license, dependency groups, and prefix-substituted caveats |
| JSON `info` | Pretty JSON v2 wrapper `{"formulae":[…],"casks":[]}` over borrowed merged raw API objects; named argument order; no-name catalog payload order |
| `search` | `/regex/` matching with exact invalid-regex refusal; simplified lowercase substring matching after stripping outside `[a-z0-9@+]`; formula names and aliases plus additive descriptions; cask tokens plus additive descriptions; canonical deduplication, sorting, filters, section headers, blank separator, deterministic columns, and query-naming empty-result refusal |
| `desc` | Alias/oldname-aware canonical formula lookup, argument-order output, null-description omission, and typed `MissingFormula` |

All user output goes through `Reporter`. The operations acquire no locks, perform no HTTP requests or host commands, and make no filesystem mutations.

## Tests and exact gate output

New discovery integration tests: **12 passed**:

- `tests/list.rs`: 3
- `tests/info.rs`: 4
- `tests/search.rs`: 3
- `tests/desc.rs`: 2

The tests cover column-major boundaries, width `0`, one-column fallback, empty state, semantic versions, linked-versus-latest file listing, missing kegs, full not-installed and installed info, revisions, keg-only and host-bottle markers, caveats, dependency groups, GitHub source URLs, raw JSON deep equality and all-catalog order, substring/alias/punctuation/regex matching, invalid regex, additive descriptions, formula/cask filters, empty search, and description hit/null/order/missing behavior. Scratch-prefix fingerprints prove read-only behavior; injected command runners panic on any host command.

Final required gate, run after the last source edit:

```text
$ cargo fmt -p zapbrew-ops
exit 0

$ cargo test -p zapbrew-ops
104 passed; 0 failed; 0 ignored; 19 test binaries plus the doc-test runner (20 runners total)
Doc-tests: 0 passed; 0 failed; 0 ignored

$ cargo clippy -p zapbrew-ops --all-targets -- -D warnings
exit 0; 0 warnings
```

## Self-review

- Confirmed the edited source and test paths match Task 6d; no install, lifecycle, lower-crate, CLI, or reference file changed.
- Confirmed list and info scan immutable installed-state snapshots and never acquire formula locks.
- Confirmed named list resolution falls back to a validated raw installed name only after catalog exact/alias/oldname resolution misses.
- Confirmed installed file output excludes directories, includes symlinks, remains absolute and sorted, and selects linked before latest.
- Confirmed the info title uses the source `Version` while installed lines use full revision-bearing `PkgVersion` values.
- Confirmed bottle labeling uses the current net bottle selector, including compatible and universal bottle fallback.
- Confirmed JSON v2 serializes borrowed raw values without rebuilding or dropping unknown API fields.
- Confirmed description search adds to name/token matching rather than replacing it and canonical results cannot duplicate.
- Confirmed no stdout/stderr writes, network calls, host commands, locks, unsafe code, `unwrap`, TODO, compatibility shim, or duplicated size/prefix formatting logic was introduced.
