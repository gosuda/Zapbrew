# Cask fetch stabilization report

## Result

Zapbrew `fetch` now downloads cask artifacts from the signed cask catalog without extracting, installing, or executing them. The command supports formula-only, cask-only, and formula-first automatic selection.

Commit: `a5c6e1a feat(fetch): download cask artifacts`

## Contract

- `fetch` requires at least one name.
- `--formula`/`--formulae` and `--cask`/`--casks` are mutually exclusive.
- `--cask` conflicts with `--deps`.
- Automatic selection resolves a formula exact name, alias, or old name before a cask token or old token. A cask hit does not trigger formula-migration network I/O.
- `--deps` expands formula dependencies only. An automatically selected cask remains one download target.
- Formula and cask canonical names use separate duplicate sets.
- Missing cask URL or checksum reports a warning and skips the unavailable host variant. Invalid token, version, or checksum fails before effects.
- Literal `no_check` forces a remote transfer. If another request for the same URL declares a checksum, that checksum gates publication.

## Network and cache boundary

- Cask downloads use typed artifact requests. They never use bottle-specific GHCR authorization or bottle-domain rewriting.
- Preparation parses every URL, accepts only HTTP or HTTPS, validates the URL basename and requested alias, rejects cross-URL alias collisions, and inspects static final, incomplete, ancestor, and non-replaceable alias states before any network work.
- One URL owns one transfer and one content file under `downloads/<sha256(url)>--<source-basename>`.
- Request-specific aliases use `<token>--<version><ext>` and preserve input ordering.
- Conflicting declared checksums for one URL fail during preparation.
- The downloader computes the actual SHA-256 for every artifact, including `no_check` downloads.
- A forced transfer verifies any declared checksum before atomic publication. A mismatch removes only the incomplete file and preserves the prior final file and aliases.

The filesystem checks cover static state. They do not claim protection against a separate same-user process that swaps a path between adjacent system calls.

## Shared cask metadata

Cask token, version, URL, checksum, and cache-alias validation moved from install transaction internals into one cask-module owner. Cask install retains its prior refusal behavior. Fetch treats only missing URL or checksum as an unavailable host variant.

## Changed surfaces

- `crates/zapbrew-cli/src/cli.rs`
- `crates/zapbrew-cli/src/dispatch.rs`
- `crates/zapbrew-cli/assets/completions/{bash,zsh,fish}`
- `crates/zapbrew-net/src/{cache,download,lib,types}.rs`
- `crates/zapbrew-net/tests/download.rs`
- `crates/zapbrew-ops/src/cask/{install,mod,transaction}.rs`
- `crates/zapbrew-ops/src/fetch.rs`
- `crates/zapbrew-ops/tests/fetch.rs`
- `docs/commands.md`

## Evidence

Focused gate after the final fixes:

- `cargo fmt --all -- --check`: passed.
- `cargo clippy -p zapbrew-net -p zapbrew-ops -p zapbrew-cli --all-targets -- -D warnings`: passed.
- `cargo test -p zapbrew-net --test download`: 38 passed.
- `cargo test -p zapbrew-ops --test fetch --test cask_install`: 69 passed.
- `cargo test -p zapbrew-cli --bin zapbrew`: 94 passed.
- `cargo test -p zapbrew-cli --test command_compat command_reference_matches_binary_help`: passed.

Workspace gate:

- `cargo build --workspace`: passed.
- `cargo clippy --workspace --all-targets -- -D warnings`: passed.
- `cargo test --workspace`: 887 passed in 70 suites; 2 ignored.

Review gates:

- Standard review: PASS after alias-directory preflight was added.
- Security review: PASS with zero surviving findings under the stated threat model.
- Mandatory alternate review: PASS after the shared-URL representative position/request-index defect was fixed and pinned by regression coverage.

## Deliberate divergence

Homebrew can freshness-check a cached `no_check` cask through remote metadata. Zapbrew conservatively transfers it again. It does not trust unverifiable local bytes as fresh.

## Unresolved prerequisites

The stabilization ledger remains `VALID / INCOMPLETE`. Authenticated maintainer scope approval, authenticated tranche review and landing on `main`, performance disposition, and native-platform evidence are separate gates. This report does not claim them.