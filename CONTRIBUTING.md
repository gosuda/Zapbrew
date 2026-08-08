# Contributing

## Toolchain

Rust 1.97.1, pinned in `rust-toolchain.toml`. With `rustup` installed, the
correct toolchain is selected automatically inside the repository. Do not raise
`rust-version` in `Cargo.toml` without a reason recorded in `GROUNDING.md`.

## Checks

Run all three before opening a pull request:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets
cargo test --workspace
```

There is no CI workflow in this repository yet, so these checks are the gate.

## Rules the build enforces

The workspace sets these lints for every crate:

- `unsafe_code = "forbid"`. There is no `unsafe` block anywhere, and a new one
  will not compile.
- `clippy::unwrap_used = "deny"`. Return a typed error instead.

Libraries use `thiserror` enums. `anyhow` is confined to the CLI entry point.

## Dependencies

Every dependency is pinned in the workspace `[workspace.dependencies]` table and
justified in [`GROUNDING.md`](GROUNDING.md) with its release date and the reason
it was chosen. When you add or change one:

1. Check the current version on crates.io rather than relying on memory.
2. Add a row to `GROUNDING.md`.
3. Reject anything with no release in the last 12 months. The `Removed pins`
   section records prior cases.

TLS goes through `rustls`. `reqwest` is configured with default features off.

## Tests

Tests assert observable behavior: command output, files on disk, HTTP requests,
and error text. Use `wiremock` for HTTP boundaries and `tempfile` for isolated
prefixes, so no test touches a real Homebrew installation. `insta` snapshots
cover user-visible output only.

A test earns its place if removing it would let a real defect ship.

## Commits

Conventional commits with crate scopes, matching the existing history:

```
feat(cli,ops): add postinstall subcommand
fix(ops): postinstall journal lifecycle, tap_info symlink and measure
test(ops): cover postinstall rollback/cleanup and install EdgeFilter
```

One concern per commit. Keep unrelated changes in separate commits.

## Scope

Zapbrew consumes bottles. It does not build from source and has no
formula-authoring commands. See
[What Zapbrew does not do](README.md#what-zapbrew-does-not-do). Proposals that
require evaluating Ruby formulae are out of scope.

`.references/brew` is a read-only checkout of Homebrew used to confirm behavior.
It is not part of the build and is git-ignored.
