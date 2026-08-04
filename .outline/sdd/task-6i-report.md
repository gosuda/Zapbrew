# Task 6i — services and update report

## Scope

Implemented only the Task 6i surfaces: `RefreshReport`, the existing workspace `plist` dependency in `zapbrew-ops`, additive platform command specs, services/update operations and test seams, and the two focused integration suites.

## Refresh report

| Prior verified cask payload | New verified payload | `casks_changed` |
|---|---|---:|
| absent | any verified payload | `false` |
| present and byte-identical | same payload | `false` |
| present and different | replacement payload | `true` |

Formula object-count behavior remains unchanged. The API tests exercise first fetch, unchanged casks, changed casks, and the existing formula object-count case.

## Render table

| Platform/artifact | Required rendering proof |
|---|---|
| Linux `.service` | Exact `[Unit]`, `[Install]`, and `[Service]` snapshot; `simple`/`oneshot`; safely double-quoted argv; keep-alive restart; delay/timeout/nice/path fields; sorted environment keys |
| Linux `.timer` interval | Exact timer snapshot with `Unit=homebrew.<name>.service` and `OnUnitActiveSec=` |
| Linux `.timer` cron | Exact timer snapshot with `Persistent=true` and translated `OnCalendar=` |
| macOS `.plist` | Exact XML snapshot with label, argv, run-at-load, keep-alive, session types, interval, working/log paths, and sorted environment keys |
| Placeholders | `$HOMEBREW_PREFIX`, `$HOMEBREW_CELLAR`, `/$HOME`, and opt paths substitute; object runs, unknown run values/types, and leftover `$HOMEBREW_*`/`@@...@@` refuse by formula name |

The launchd renderer calls the pinned `plist` 1.10 API directly: `plist::to_writer_xml(&mut output, &values)`. The API signature was verified in the installed 1.10.0 source at `/home/alpha/.cargo/registry/src/index.crates.io-1949cf8c6b5b557f/plist-1.10.0/src/ser.rs:792-794`. The focused test parses the emitted XML back through `plist::Value::from_reader_xml` and compares the complete serialized XML snapshot, including deterministic `ALPHA` then `ZETA` environment ordering.

## Service state table

| Action/state | Result |
|---|---|
| Start, not installed | `Formula \`<name>\` is not installed.` refusal; no service command |
| Start, no API service | Named no-service refusal |
| Start, already active | `print:Service \`<name>\` already started, use zapbrew restart <name> to restart.`; no write/reload/start |
| Start, inactive | Write generated file(s), reload systemd when applicable, start/load, then success `ohai` |
| Stop, inactive | `opoo:Service \`<name>\` is not started.` |
| Stop, active | Stop/unload, then success `ohai` |
| Restart | Active stop/unload followed by regenerated start/load; both success messages |
| List | Literal `Name Status File` header; installed service formulae only; sorted names; `started`, `stopped`, or `none`; no uid lookup |
| `BottleTag::All` | Named unsupported-platform refusal |

## Exact service argv and output proof

Linux start:

```text
systemctl --user is-active homebrew.demo.service
systemctl --user daemon-reload
systemctl --user start homebrew.demo.service
ohai:Successfully started `demo` (label: homebrew.demo)
```

Timed Linux start selects the timer:

```text
systemctl --user is-active homebrew.demo.timer
systemctl --user daemon-reload
systemctl --user start homebrew.demo.timer
```

Linux stop/restart uses the same injected status probe and exact `systemctl --user stop ...` argv before regenerating and starting. macOS uses:

```text
launchctl list homebrew.mxcl.demo
launchctl load <home>/Library/LaunchAgents/homebrew.mxcl.demo.plist
launchctl list homebrew.mxcl.demo
launchctl unload <home>/Library/LaunchAgents/homebrew.mxcl.demo.plist
```

Success output is exact:

```text
ohai:Successfully started `demo` (label: homebrew.mxcl.demo)
ohai:Successfully stopped `demo` (label: homebrew.mxcl.demo)
```

## Update table

| Formula objects changed | Cask payload changed | Tap HEAD changed | Output after `ohai:Updating Homebrew...` |
|---:|---:|---:|---|
| 0 | no | 0 | `print:Already up-to-date.` |
| 0 | yes | 0 | no additional row |
| 1 | no | 0 | `ohai:Updated Formulae`; `print:Updated 1 formula.` |
| 3 | no | 0 | `ohai:Updated Formulae`; `print:Updated 3 formulae.` |
| 0 | no | 1 | `print:Updated 1 tap (acme/tools).` |
| 2 | yes | 2 | sorted tap summary, then formula heading/count |
| any | any | read/pull failure | warning for that tap, continue remaining taps, return success |

Each real tap records the exact injected sequence:

```text
git -C <tap> rev-parse HEAD
git -C <tap> pull --ff-only --quiet
git -C <tap> rev-parse HEAD
```

The multi-tap proof emits:

```text
ohai:Updating Homebrew...
print:Updated 2 taps (alpha/tools, zeta/extra).
ohai:Updated Formulae
print:Updated 2 formulae.
```

The scan uses the existing two-level real-directory tap discovery, additionally requires a real `.git` directory, and skips tap and `.git` symlinks. Tests use only scratch directories and scripted `CommandRunner` implementations; no network, host Git, systemd, or launchd runs.

## Verification

Executed after the final source/test edit:

```text
cargo fmt --all && cargo test -p zapbrew-api && cargo test -p zapbrew-ops --test services --test update && cargo clippy -p zapbrew-api -p zapbrew-ops --all-targets -- -D warnings
```

Result:

```text
cargo test -p zapbrew-api: 51 passed (2 suites)
cargo test -p zapbrew-ops --test services --test update: 16 passed (2 suites)
cargo clippy -p zapbrew-api -p zapbrew-ops --all-targets -- -D warnings: OK
```

## Self-review

- Every command path crosses the injected `CommandRunner`; every message crosses `Reporter`.
- Service files and tap repositories are confined to typed environment roots; symlinked tap entries and `.git` paths are never followed.
- Rendering and output order are deterministic: `BTreeMap` environment keys, installed-state name order, and sorted changed taps.
- Cask-only refreshes suppress `Already up-to-date.` without inventing an unapproved cask output row.
- No CLI, tap verb, cask operation, shim, Ruby path, unsafe code, fallback compatibility path, or dependency beyond the existing workspace `plist` crate was added.
- The unrelated untracked scratch files were left untouched.
