# Task 6h report — shellenv, tap, untap, and tap-info

## Name and path model

| Input | Display name | Filesystem path below `$HOMEBREW_LIBRARY/Taps` | Default remote |
|---|---|---|---|
| `Homebrew/homebrew-core` | `homebrew/core` | `Homebrew/homebrew-core` | `https://github.com/Homebrew/homebrew-core` |
| `homebrew/cask` | `homebrew/cask` | `Homebrew/homebrew-cask` | `https://github.com/Homebrew/homebrew-cask` |
| `linuxbrew/homebrew-core` | `linuxbrew/core` | `Linuxbrew/homebrew-core` | `https://github.com/Linuxbrew/homebrew-core` |
| `Acme/homebrew-Tools` | `acme/tools` | `acme/homebrew-tools` | `https://github.com/acme/homebrew-tools` |

Names must have exactly two non-empty slash-separated parts; `.` and `..` path components are refused. The model lowercases display names, removes one leading `homebrew-` from the repository, and reserves `Homebrew`/`Linuxbrew` capitalization for filesystem and remote identity.

## Exact output samples

For an environment whose prefix and repository are `/opt/homebrew`, Bash shellenv is one `Reporter::print` payload:

```text
export HOMEBREW_PREFIX="/opt/homebrew";
export HOMEBREW_CELLAR="/opt/homebrew/Cellar";
export HOMEBREW_REPOSITORY="/opt/homebrew";
export PATH="/opt/homebrew/bin:/opt/homebrew/sbin${PATH+:$PATH}";
[ -z "${MANPATH-}" ] || export MANPATH=":${MANPATH#:}";
export INFOPATH="/opt/homebrew/share/info:${INFOPATH:-}";
```

Tap listing and successful tap/untap snapshots exercised by the focused tests:

```text
acme/tools
homebrew/zeta
```

```text
==> Tapping acme/tools
Tapped (2 files, 1.5KB).
==> Untapping acme/tools
Untapped (2 files, 1.5KB).
```

Core/cask refusal:

```text
Tapping homebrew/core is no longer typically necessary.
Add --force if you are sure you need it for contributing to Homebrew.
```

Tap summary:

```text
2 taps, 0 private, 2 formulae, 1 commands, 2.5KB
```

Installed tap block, including a non-default branch:

```text
acme/alpha: Installed
1 command, 1 cask, 1 formula
/opt/homebrew/Library/Taps/acme/homebrew-alpha (4 files, 2KB)
origin: ssh://git@example.test/acme/alpha
HEAD: 0123456789abcdef
last commit: 2 days ago
branch: feature/work
```

Stable git fallback block:

```text
acme/tools: Installed
No commands/casks/formulae
/opt/homebrew/Library/Taps/acme/homebrew-tools (0 files, 0B)
origin: (none)
HEAD: (none)
last commit: never
branch: (none)
```

Missing and JSON refusal outputs:

```text
zeta/missing: Not installed
```

```text
tap-info JSON output is unavailable without Ruby.
```

## Recorded git argv

Default clone:

```text
git -c core.hooksPath=/dev/null clone --origin=origin --template= --config core.fsmonitor=false --end-of-options https://github.com/Homebrew/homebrew-core /opt/homebrew/Library/Taps/Homebrew/homebrew-core
```

Custom clone:

```text
git -c core.hooksPath=/dev/null clone --origin=origin --template= --config core.fsmonitor=false --end-of-options ssh://git@example.test/acme/tools /opt/homebrew/Library/Taps/acme/homebrew-tools
```

Each installed tap-info block records these injected reads, in order:

```text
git -C /opt/homebrew/Library/Taps/acme/homebrew-alpha config --get remote.origin.url
git -C /opt/homebrew/Library/Taps/acme/homebrew-alpha rev-parse HEAD
git -C /opt/homebrew/Library/Taps/acme/homebrew-alpha log -1 --format=%cr
git -C /opt/homebrew/Library/Taps/acme/homebrew-alpha symbolic-ref --short HEAD
```

## Verification

Command run exactly:

```text
cargo fmt -p zapbrew-ops && cargo test -p zapbrew-ops --test shellenv --test tap --test untap --test tap_info && cargo clippy -p zapbrew-ops --all-targets -- -D warnings
```

`cargo fmt` emitted no output. Focused test output:

```text
running 3 tests
test explicit_shell_wins_and_detected_login_shell_uses_its_basename ... ok
test unknown_and_absent_values_fall_back_to_bash ... ok
test maps_every_supported_shell_and_alias_to_the_existing_template ... ok
test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out

running 4 tests
test forced_core_and_third_party_clones_record_exact_default_and_custom_argv ... ok
test core_and_cask_require_force_before_any_output_or_git_call ... ok
test no_name_lists_only_real_taps_in_sorted_order ... ok
test invalid_existing_and_failed_clones_are_stable_and_do_not_run_host_git ... ok
test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out

running 4 tests
test named_taps_are_sorted_missing_taps_are_all_reported_and_json_refuses_early ... ok
test summary_counts_real_installed_tap_content_without_git_or_ruby ... ok
test installed_flag_prints_sorted_blocks_and_records_every_git_argv ... ok
test git_failures_use_stable_fields_and_never_fail_tap_info ... ok
test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out

running 4 tests
test missing_non_directory_and_symlink_taps_are_refused_without_traversal ... ok
test removes_symlink_entries_without_following_their_target_outside_taps ... ok
test parses_every_name_before_removing_any_tap ... ok
test removes_taps_in_input_order_and_prunes_only_the_empty_real_user_directory ... ok
test result: ok. 4 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out
```

Clippy completed with:

```text
Finished `dev` profile [unoptimized + debuginfo] target(s) in 13.02s
```

The chained gate exited successfully.

## Self-review

- The diff is restricted to the four new operation modules, their four integration suites, `src/lib.rs`, and this ignored report.
- Shell selection covers every requested mapping, explicit precedence, login-shell basename handling, and unknown/absent Bash fallback while reusing `Env::shellenv` unchanged.
- Tap discovery and measurement sort directory entries, inspect entries with `symlink_metadata`, and never traverse tap/user symlinks.
- Untap removes only a validated real tap directory, preserves symlink targets and the Cellar, and prunes only an empty real user directory.
- Clone and tap-info git calls use the injected `CommandRunner`; tests issue no host git or network calls and assert argv exactly.
- Production output uses only `Reporter`; the changed files contain no direct process launch, `unsafe`, `.unwrap()`, `TODO`, or stdout/stderr macros.
