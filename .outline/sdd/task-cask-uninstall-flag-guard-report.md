# Cask uninstall flag-guard report

## Result

The cask uninstall dispatch path no longer silently drops `--force` or `--ignore-dependencies`. Zapbrew refuses both flags before catalog loading or operation effects because their Homebrew cask semantics are not implemented.

Commit: `8332896 fix(cli): refuse unsupported cask uninstall flags`

## Argument accounting

| CLI field | Cask-path disposition |
| --- | --- |
| `names` | Mapped to `cask::uninstall::Args.tokens`. |
| `zap` | Mapped to `cask::uninstall::Args.zap`. |
| `cask` | Selects the cask branch. |
| `formula` | Selects the formula branch and conflicts with `cask`. |
| `force` | Refused before effects. |
| `ignore_dependencies` | Refused before effects. |

The formula branch continues to map `force` and `ignore_dependencies` to formula uninstall unchanged.

## Homebrew reference

Homebrew 6.0.16 gives both refused flags observable cask behavior:

- `--force` reaches cask installer uninstall/zap phases and affects absent-cask and per-artifact error handling.
- `--ignore-dependencies` skips the installed-cask dependent check, including recursive cask requirements.

Zapbrew does not model either complete semantic. Partial support would still lie. One table-driven guard names and refuses the first unsupported flag.

## Platform boundary

Linux cask uninstall remains refused at the operation entry point before host commands or Caskroom mutation. Supported cask invocations still reach that platform refusal. Unsupported flag combinations fail earlier in dispatch on every platform.

## Changed surfaces

- `crates/zapbrew-cli/src/dispatch.rs`
- `crates/zapbrew-ops/tests/cask_uninstall.rs`

## Evidence

- `cargo test -p zapbrew-ops --test cask_uninstall`: 16 passed.
- `cargo test -p zapbrew-cli --bin zapbrew dispatch::tests::uninstall`: 2 passed.
- `cargo clippy -p zapbrew-ops -p zapbrew-cli --all-targets -- -D warnings`: passed.
- Standard review: PASS with zero findings.
- Mandatory alternate review: PASS with zero findings.

Regression coverage loops over both unsupported flags and asserts each refusal names the flag and `--cask`. Formula routing and cask `--zap` mapping remain pinned. A Linux integration test asserts refusal before commands and filesystem effects.

## Deliberate divergence

Zapbrew refuses the two flags instead of implementing only part of Homebrew's semantics. This follows decision `D8-shared-flag-contracts`: no shared flag may be silently dropped.

## Unresolved prerequisites

Exact Homebrew cask `--force` and cask-dependent graph behavior remain future coherent behavior tranches. Authenticated maintainer review, landing, ledger evidence, performance disposition, and native macOS proof remain separate gates.