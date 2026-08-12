# Cask upgrade report

## Result

Zapbrew upgrades installed formulae and reversible casks through one command path. Explicit selectors and formula-first auto mode share deterministic target resolution. Named and greedy cask policies are evaluated before canonical token locks are acquired; the selected casks are then rescanned and reselected under those locks. Every selected formula and cask artifact downloads before the first install transaction.

Feature commit: `9e2faf4 feat(cask): upgrade reversible artifacts`

Generated command surfaces commit: `8049a61 docs(cli): refresh upgrade command surfaces`

## Changed symbols

- `zapbrew_cli::cli::Commands::Upgrade` and its formula/cask, appdir, and greedy flags.
- `zapbrew_cli::dispatch` upgrade routing and mode validation.
- `zapbrew_ops::upgrade::{run, resolve_targets, select_named_cask, enumerate_installed, enumerate_installed_casks}`.
- `zapbrew_ops::cask::reinstall::{LockedTokens, LockedCasks, ValidatedCasks, DownloadedCasks}` and their prepare, validate, download, and cached-execution transitions.
- Cask artifact, install, and transaction support for replacement from a retained cached artifact.
- Upgrade integration coverage for mixed formula/cask selection, greedy policy, dry-run, lock ordering, validation, download ordering, cached-artifact association, and failure boundaries.

## Behavioral contract

- `--formula` and `--cask` are mutually exclusive.
- `--appdir` is accepted only for cask-only upgrades.
- Named casks use greedy outdated checks. Unnamed casks honor `--greedy`, `--greedy-latest`, and `--greedy-auto-updates`.
- Dry-run performs no locking, network access, or mutation.
- Shared tap locks precede formula locks; cask token locks follow the formula lock tier.
- Cask records and outdated status are rescanned under canonical token locks.
- Cask download requests are validated before formula preinstall migration I/O.
- All formula and cask artifacts download before formula or cask install transactions.
- Downloaded casks retain their matching cached artifacts until journaled replacement execution.
- Linux formula preinstall refreshes `ld.so` and preferred GCC library links before upgrade installation.

## Evidence

- Focused upgrade suite: 19 tests passed after the final feature correction.
- Final stack `cargo fmt --all -- --check`: passed.
- Final stack `cargo clippy --workspace --all-targets -- -D warnings`: passed.
- Final stack `cargo test --workspace`: 929 passed, 2 ignored.
- macOS cross-checks for `aarch64-apple-darwin` and `x86_64-apple-darwin`: passed.
- Release build: passed.
- Live Linux GNU Wget 1.25.0 install, receipt, and uninstall proof passed with 11 runtime dependencies, 11 automatic removals, zero broken symlinks, and an empty Cellar.
- Standard review: PASS.
- Security review: PASS.
- Mandatory alternate review found the missing Linux preinstall calls; the correction was applied and the alternate recheck passed.
- Atomic split review accepted `9e2faf4` as executable behavior and `8049a61` as generated command-surface documentation.

## Unresolved prerequisites

- Native macOS runtime proof still requires a macOS host. Cross-compilation is not native effect evidence.
- Repository-maintainer scope, decision, and applicable safety-deviation approvals must be authenticated. Agents must not create them.
- The exact tranche requires authenticated review, publication consent, and landing on `main` before the ledger can record it as reviewed and landed.
