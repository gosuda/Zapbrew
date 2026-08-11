# Linux reversible cask-install report

## Result

The D2/D5 tranche opens Linux cask installation for a closed, reversible,
data-only artifact set. Linux cask uninstall remains refused. Zapbrew does not
execute catalog-supplied scripts, flight blocks, serialized install steps, or
manual installers.

## Accepted Linux surface

- `binary`, `manpage`, and `appimage`: confined symlinks.
- `artifact`, `font`, and shell completions: confined copies.
- `stage_only: [true]`: promote the staged payload without a deploy action.
- `uninstall` and `zap`: validate and store data only. Linux cannot execute the
  directives because its uninstall entry point remains behind `require_macos`.

The planner rejects macOS-only artifacts, Linux disk images, cask-supplied
execution, unknown kinds, multiple directive kinds, ignored modifiers, malformed
targets, and malformed removal directives before locks, network access, or
staging. macOS keeps its existing `app`, `suite`, plugin, and fixed
`/usr/sbin/installer` package routes. macOS rejects Linux-only `appimage` and
keeps the fixed unsupported-manual-installer result.

## Trust boundaries

- The API parser preserves `appimage`, `suite`, `disabled`, and
  `disable_reason`. Install refuses disabled casks before effects.
- Token and version must each be one non-empty normal path component before
  lock acquisition and at transaction sinks.
- Artifact planning is closed. Each accepted kind has one allowed key set and
  one value schema. Catalog and stored-record `uninstall`/`zap` data use the
  same predicate.
- URL suffix normalization strips query and fragment data, decodes valid percent
  escapes once, folds case, and feeds both Linux `.dmg` preflight and archive
  dispatch.
- Caskroom path, directory, and regular-file helpers use `symlink_metadata`.
  They reject out-of-tree paths, symlink leaves, symlink ancestors,
  non-directory ancestors, non-regular write leaves, and metadata errors.
- Staged install-record and receipt writes recheck the regular-file sink
  immediately before `fs::write`. A payload symlink named
  `.zapbrew-record.json` cannot redirect the write.
- External deploy targets cannot enter the managed Caskroom. Internal backup
  paths use the Caskroom-specific guard instead of the external-target guard.
- Deterministic cache `.incomplete` paths and every existing cache ancestor are
  checked without following symlinks before resume inspection and immediately
  before open. The artifact and bottle routes share this check.
- The transaction records the highest newly created deploy-parent and
  receipt-parent tree. Rollback removes new receipt state before it restores
  old same-version metadata, then replays the artifact journal.
- Refusal completes before `acquire_locks`; early-refusal tests prove that an
  absent locks directory remains absent.

These checks cover the existing static filesystem state at each sink. They do
not claim protection from a separate same-user process that swaps an ancestor
between adjacent system calls. Closing that race requires a dirfd/openat design
and is outside this tranche.

## Files

- `crates/zapbrew-api/src/lib.rs`
- `crates/zapbrew-api/src/model.rs`
- `crates/zapbrew-net/src/download.rs`
- `crates/zapbrew-net/tests/download.rs`
- `crates/zapbrew-ops/src/cask/archive.rs`
- `crates/zapbrew-ops/src/cask/artifact.rs`
- `crates/zapbrew-ops/src/cask/install.rs`
- `crates/zapbrew-ops/src/cask/mod.rs`
- `crates/zapbrew-ops/src/cask/transaction.rs`
- `crates/zapbrew-ops/src/cask/uninstall.rs`
- `crates/zapbrew-ops/tests/cask_install.rs`
- `.outline/ledger/scope.json`
- `.outline/sdd/task-linux-cask-install-brief.md`
- `.outline/sdd/task-linux-cask-install-report.md`

## Regression coverage

The tranche pins:

- every accepted Linux artifact and `stage_only`;
- every macOS-only and execution-bearing refusal before I/O;
- malformed, unknown, mixed, and ignored artifact input;
- disabled casks, unsafe token/version, and unsafe appdir/target paths;
- `.dmg` query, fragment, case, and percent-encoding forms;
- staged record, appdir, Caskroom token, Caskroom metadata, cache leaf, and cache
  ancestor symlinks;
- deploy-parent and receipt-parent ownership on rollback;
- force reinstall and cross-version restoration;
- macOS artifact behavior and Linux uninstall refusal.

## Verification

Focused verification:

- `cargo test -p zapbrew-ops --test cask_install`: 51 passed.
- `cargo test -p zapbrew-ops --test cask_uninstall --test cask_list`: 23 passed.
- `cargo test -p zapbrew-net --test download`: 18 passed.
- `cargo test -p zapbrew-api`: 53 passed.
- `cargo clippy -p zapbrew-api -p zapbrew-net -p zapbrew-ops --all-targets -- -D warnings`: passed.

Workspace gate:

- `cargo fmt --all -- --check`: passed.
- `cargo build --workspace`: passed.
- `cargo clippy --workspace --all-targets -- -D warnings`: passed.
- `cargo test --workspace`: 844 passed in 70 suites; 2 ignored.

Ledger verification returned `VALID / INCOMPLETE` with exit 1. The remaining
incompleteness is expected: authenticated maintainer approval, unresolved
repository-wide evidence cells, reviewed landing, and native-platform evidence
are separate gates. It returned no structural error.

Security review returned PASS with zero findings after tracing catalog input to
cache, extraction, deploy, record, receipt, force replacement, rollback, and
Linux-uninstall refusal.

The final standard reviewer reported one appdir-root finding. The finding is
invalid in the current implementation: for a target below the appdir, the
ancestor loop inspects the selected root on its last iteration; the install
preflight uses `appdir/zapbrew-appdir-probe`, so it also inspects the root. A
target equal to a symlinked appdir is rejected by `ensure_absent` and removal
uses `symlink_metadata`, so it removes the link rather than following it.

Mandatory alternate review returned PASS with zero actionable findings after
an exhaustive source, test, brief, report, and ledger audit.
