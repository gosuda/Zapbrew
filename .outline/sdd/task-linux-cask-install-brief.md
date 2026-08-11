# Linux cask install brief

## Objective

Open Linux cask install for the closed, reversible artifact subset. Keep Linux uninstall refused.

This is one externally complete D2/D5 tranche. The Linux install gate must not open until classification, container preflight, reversible effects, rollback tests, and no-effect refusal tests all exist in the same commit.

## Anchor and permanent invariant

Carmack anchors the tranche: one effect and rollback domain, one commit.

`no-cask-supplied-execution` is permanent. Zapbrew does not run cask-supplied Ruby or shell code, request cask-supplied sudo execution, or interpret serialized InstallSteps. This is an outside-architecture refusal, not deferred work. It does not remove the existing macOS `pkg` action through the fixed `/usr/sbin/installer` boundary.

Refuse on every platform:

- `installer` with `script:`;
- `preflight`, `postflight`, `uninstall_preflight`, and `uninstall_postflight`;
- serialized cask InstallSteps or equivalent lifecycle execution.

Refuse manual installers on Linux with Homebrew's requires-macOS text.

## Contract

PRE:

- The cask came from the verified JSON catalog, but all catalog fields remain untrusted.
- The CLI approved `appdir`.
- No download, staging directory, or target mutation exists for this operation.

POST:

- `artifact::plan` returns a complete plan only when every artifact has one recognized directive kind, a valid value schema, a platform-supported effect, and a reversal disposition.
- Any unknown, malformed, mixed, macOS-only, manual, or execution-bearing cask is refused before network or filesystem effects.
- A Linux `.dmg` URL is refused before download.
- A successful Linux install uses the existing confined transaction and atomic Caskroom promotion.

INVARIANT:

- Linux plans contain only reversible link/copy deployment or `stage_only` with no deployment action.
- Linux uninstall remains behind `require_macos` for its separate directive and rollback tranche.
- macOS behavior remains unchanged.
- Caskroom effects use shared path, directory, and regular-file no-follow
  helpers. Existing static symlink or non-directory ancestors under the
  Caskroom are refused at each sink. This tranche does not claim protection
  against a concurrent same-user process swapping a path between adjacent
  syscalls.

## Artifact table

Linux accepts:

- `binary`: executable symlink under the prefix;
- `manpage`: symlink under the prefix man tree;
- `appimage`: executable symlink under `~/Applications`;
- `artifact`: confined copy;
- `font`: confined copy under `~/Library/Fonts`;
- `zsh_completion`, `bash_completion`, `fish_completion`: confined copies under prefix completion roots;
- `stage_only: true`: stage and promote without a deployment action;
- `uninstall` and `zap`: validate and store directives only. Execution remains inaccessible because Linux uninstall stays refused.

Linux refuses with `<token>: This cask requires macOS.`:

- a top-level macOS dependency;
- `app`, `suite`, `pkg`, `service`, `colorpicker`, `dictionary`, `input_method`, `internet_plugin`, `keyboard_layout`, `prefpane`, `qlplugin`, `mdimporter`, `screen_saver`, `audio_unit_plugin`, `vst_plugin`, and `vst3_plugin`;
- a manual installer.

Execution-bearing and unknown artifacts use the existing fixed text: `Cask '<token>' uses unsupported artifact '<kind>'.`

An object with more than one recognized directive kind uses: `Cask '<token>' artifact declares multiple directives; exactly one is required.`

A Linux disk image uses: `Cask '<token>' ships a macOS disk image, which is unavailable on Linux.`

Refusal precedence is requires-macOS, execution-bearing, unknown, malformed, then Linux disk-image container after a valid plan.

## Required implementation

- Keep the classifier in `crates/zapbrew-ops/src/cask/artifact.rs`. `artifact::plan` is the single raw-artifact-to-plan compiler. Do not create a second classifier module.
- Add explicit `appimage` recognition to `crates/zapbrew-api/src/model.rs`.
- Enforce exactly one recognized directive kind and every top-level, nested, target, and uninstall/zap value schema before building actions. Refuse ignored modifiers instead of accepting them.
- Make the plan platform-aware. Preserve current macOS mappings, including the fixed `pkg` execution boundary.
- Add `appimage` and `stage_only` actions through existing reversal machinery.
- Remove `require_macos` only from `cask/install.rs`. Keep it in `cask/uninstall.rs`.
- Use one shared URL-suffix normalizer for install preflight and archive dispatch. Strip query and fragment, decode percent escapes, and refuse Linux `.dmg` before download. Malformed percent escapes must not be able to produce a recognized `.dmg` suffix.
- Complete all pure validation before lock acquisition. A refusal must not create the locks directory.
- Validate cask token and version as one normal path component before locks, download, receipt creation, staging, or promotion. Recheck at transaction sinks.
- Use one shared no-follow target-confinement policy for planning, apply, force backup, record validation, and rollback. Existing symlink ancestors under approved lexical roots must not redirect effects outside those roots.
- Add one shared Caskroom no-follow helper and call it immediately before every create/write/rename/remove/backup/receipt path under Caskroom, including token directories and `.metadata`.
- No-follow validate the deterministic cache `.incomplete` file and every
  existing ancestor before length inspection and immediately before open.
- Track receipt-owned and deploy-owned parent trees so rollback removes new
  state before it restores old same-version metadata.
- Refine `.outline/ledger/scope.json` D2 and existing cask-root cells. Do not add a cell because the fixed ledger schema does not permit one without a result/verifier migration.
- Do not change CLI D8 flags, Linux uninstall behavior, dependencies, or macOS artifact effects.

## Tests

Add or update contract tests for:

- all reversible Linux artifacts and records;
- `stage_only: true` and malformed stage-only values;
- every macOS-only class and macOS dependency before I/O;
- script/manual installer, flight blocks, and InstallSteps before I/O;
- a supported plus unsupported mixed cask before I/O;
- Linux `.dmg` query, fragment, case, percent-encoded-dot, and malformed-percent forms with zero requests, no locks, and no staging where applicable;
- multiple recognized kind keys, unknown keys, non-string/empty/duplicate target modifiers, ignored pkg modifiers, and malformed uninstall/zap values;
- unsafe token/version path components before I/O, including an absent `env.locks` directory that stays absent after refusal;
- an approved lexical target whose existing ancestor is a symlink outside the root;
- a Caskroom token or `.metadata` ancestor that is a symlink;
- a symlinked cache `.incomplete` leaf and a symlinked `downloads` ancestor;
- a staged install-record symlink that points outside Caskroom;
- deploy-parent and receipt-parent cleanup after later failure;
- rollback after a later Linux action fails, including AppImage target cleanup;
- Linux uninstall still refusing;
- existing macOS behavior unchanged.

Refusal tests must prove zero network requests and no `.staging` creation where the refusal is expected to occur before staging, not only error text.

## Files

Expected files:

- `crates/zapbrew-api/src/model.rs`
- `crates/zapbrew-api/src/lib.rs`
- `crates/zapbrew-ops/src/cask/artifact.rs`
- `crates/zapbrew-ops/src/cask/mod.rs`
- `crates/zapbrew-ops/src/cask/install.rs`
- `crates/zapbrew-ops/src/cask/archive.rs`
- `crates/zapbrew-ops/src/cask/transaction.rs`
- `crates/zapbrew-ops/tests/cask_install.rs`
- `crates/zapbrew-net/src/download.rs`
- `crates/zapbrew-net/tests/download.rs`
- `.outline/ledger/scope.json`
- `.outline/sdd/task-linux-cask-install-report.md`

`cask/uninstall.rs` may be touched only to wire the shared removal-directive shape predicate into stored-record preflight; Linux uninstall refusal and macOS uninstall behavior must not change.

## Verification

The parent runs validation after implementation:

```text
cargo test -p zapbrew-ops --test cask_install
cargo test -p zapbrew-ops --test cask_uninstall --test cask_list
cargo test -p zapbrew-api
cargo fmt --all -- --check
cargo build --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

The mandatory alternate review gates the exact final diff. Native Linux process evidence remains a separate evidence task.