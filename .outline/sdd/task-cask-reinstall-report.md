# Cask reinstall report

## Result

Zapbrew reinstalls reversible formulae and casks through one command path. Explicit `--formula` and `--cask` selectors are supported. Auto mode resolves formulae first, then casks. Cask preflight resolves canonical tokens, acquires token locks, validates every installed record and target app directory, and rejects irreversible package or uninstall effects before formula preparation or installation work.

Feature commit: `f3edbc7 feat(cask): reinstall reversible artifacts`

## Changed symbols

- `zapbrew_cli::cli::Commands::Reinstall` and its argument surface.
- `zapbrew_cli::dispatch` reinstall routing and formula/cask argument accounting.
- `zapbrew_ops::reinstall::{run, resolve_names, prepare_formulas, execute_formulas}`.
- `zapbrew_ops::cask::reinstall::{Args, LockedCasks, Purpose, run, preflight, lock_tokens, validate}`.
- Cask install, artifact, and transaction preparation needed by reversible replacement.
- Formula and cask reinstall integration coverage.

## Behavioral contract

- `--formula` and `--cask` are mutually exclusive.
- `--appdir` is accepted only when every selected target is a cask.
- Formula-first auto resolution preserves aliases and old names before cask fallback.
- Selected casks are preflighted before formula preparation, downloads, or install transactions.
- Formula reinstalls execute before cask reinstalls to preserve formula-only output order.
- Direct cask reinstall keeps canonical token locks while each cask fetches and replaces in sequence.
- Casks with irreversible `pkg` artifacts or nonempty uninstall directives are refused instead of partially reinstalled.

## Evidence

- Final stack `cargo fmt --all -- --check`: passed.
- Final stack `cargo clippy --workspace --all-targets -- -D warnings`: passed.
- Final stack `cargo test --workspace`: 929 passed, 2 ignored.
- macOS cross-checks for `aarch64-apple-darwin` and `x86_64-apple-darwin`: passed.
- Release build: passed.
- Standard, security, and mandatory alternate reviews of the final cask replacement stack: PASS with zero remaining findings.
- Canonical ledger migration records `crates/zapbrew-ops/src/cask/reinstall.rs` and its preflight and replacement transaction cells.

## Unresolved prerequisites

- Native macOS runtime proof still requires a macOS host. Cross-compilation does not prove cask archive and application behavior through `hdiutil` and `ditto`, or native filesystem effects.
- Repository-maintainer scope, decision, and applicable safety-deviation approvals must be authenticated. Agents must not create them.
- The exact tranche requires authenticated review, publication consent, and landing on `main` before the ledger can record it as reviewed and landed.
