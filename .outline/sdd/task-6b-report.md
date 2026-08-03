# Task 6b report — install, reinstall, and fetch

## Scope

Implemented the bottle-only `install`, `reinstall`, and `fetch` verbs in `zapbrew-ops`, plus their shared formula transaction. No lower crate, CLI, reference, or unrelated verb was changed.

## Public API

- `install::Args { names, only_dependencies, force, dry_run, build_from_source, head, interactive }`
- `reinstall::Args { names }`
- `fetch::Args { names, deps }`
- Each module exports `pub async fn run(ctx: &Ctx, args: Args) -> Result<(), OpError>`.
- Transaction types remain crate-private.
- `OpError::RollbackIncomplete` carries the original typed error and every path that could not be restored or removed.

## Resolution and operation behavior

- Exact names, aliases, and oldnames resolve through `Catalog`; misses consult the async migration map before returning `MissingFormula`.
- Install rejects Ruby-dependent modes before catalog, network, lock, or prefix I/O.
- Install validates disabled/deprecated/pinned/conflicting formulae, expands pour dependencies in post-order, omits met dependencies, preserves requested/dependency receipt flags, batches downloads, and pours serially.
- Fetch optionally expands dependencies and prints deterministic `Fetching`, `Downloading`, downloaded/reused path, and SHA-256 messages without locks, cellar writes, or host commands.
- Reinstall requires installed state, preserves `installed_on_request`, stages the current bottle fully, and replaces the keg only after staging succeeds.

## Transaction state machine

1. Acquire every affected `<canonical-name>.formula.lock` in sorted order before reading the selected installed racks.
2. Create a unique staging root under the target rack on the cellar filesystem.
3. Securely unpack the bottle, relocate it with the injected `CommandRunner`, write the pretty Tab, and inventory the staged keg.
4. Unlink the previous linked keg and rename an existing target keg to its journaled backup.
5. Atomically rename the staged keg to the final version path (promotion point).
6. Link the promoted keg and its opt/linked records.
7. Copy `.bottle/etc` and `.bottle/var` skeleton entries only when absent.
8. Remove the old backup and finish.

Every transaction mutation is preceded by journal state. Rename, copy, and remove paths are confined to the Env roots; symlink and non-directory ancestors are rejected.

## Rollback proof

Rollback reverses skeleton entries, new links/records, the promoted keg, the target backup, the old link, staging, and transaction-created empty directories. A clean rollback returns the original typed error. An incomplete rollback returns `RollbackIncomplete` with all observed leftover paths.

Regression coverage proves:

- happy-path promotion, Tab fields, links, skeleton copy, caveats, and summary;
- dependency/root topological install and runtime dependency graph fields;
- exact old-keg receipt, bytes, opt record, linked record, and file-link restoration after reinstall link failure;
- relocation failure leaves no formula state;
- a mid-skeleton failure removes earlier copies, links/records, and the promoted keg while preserving pre-existing files;
- lock acquisition is canonical-name sorted and releases earlier locks after contention;
- cellar symlink ancestry is refused without outside mutation;
- already-installed, force replacement, only-dependencies, dry-run zero mutation, Ruby-mode refusals, disabled/deprecated, keg-only, reinstall-not-installed, fetch fresh/reused/deps/no-bottle.

## Verification

Exact required gate run after the final code edit:

```text
$ cargo fmt -p zapbrew-ops && cargo test -p zapbrew-ops && cargo clippy -p zapbrew-ops --all-targets -- -D warnings
cargo test: 46 passed (11 suites, 0.00s)
OK
```

Test count: **46 passed**, **0 failed**, across **11 suites**.

## Self-review

- Confirmed the diff is restricted to the Task 6b ops files, `Cargo.lock`, and this required report.
- Confirmed production code contains no stdout/stderr writes, real host command invocation, `unsafe`, `unwrap`, `TODO`, or mock transport.
- Rechecked Homebrew conflict, already-installed, fetch-path/SHA, deprecated, keg-only, caveat, and summary wording against the named reference files and Appendix F.
- Rechecked lock lifetime: install/reinstall guards remain live through download, commit, or complete rollback; fetch takes no lock.
- Rechecked failure locality: a later root failure does not roll back already committed dependency transactions.
