# Task 6k report - opt-in brew shim

## Summary
Added `zapbrew-ops::shim` (install/remove `<prefix>/bin/brew`) and a hint-name
seam on `Reporter` (`hint_program`). Shim mutations are no-follow and
identity-safe; a foreign or dangling link is never overwritten or deleted.
Three self-referential hint sites now prefix through `hint_program()`, defaulting
to `zapbrew` and switching to `brew` when a terminal reporter overrides it.

## Shim state table
Link = `<prefix>/bin/brew`; exe = canonical zapbrew executable.

| Existing state at link | `Install` | `Remove` |
| --- | --- | --- |
| missing | create symlink -> exe; print `Installed brew shim: <link> -> <exe>` | idempotent; print `No brew shim installed at <link>` |
| symlink resolving to exe (absolute) | idempotent; print `brew shim already installed at <link>` | `remove_file`; print `Removed brew shim: <link>` |
| symlink resolving to exe (relative, e.g. `../zapbrew`) | idempotent (same) | `remove_file`; print `Removed brew shim: <link>` |
| symlink to any other target | refuse `Refusing to replace existing shim: <link> -> <target>`; untouched | refuse `Refusing to remove foreign brew shim: <link> -> <target>`; untouched |
| dangling symlink | refuse (as other target); untouched | refuse (foreign); untouched |
| regular file / directory | refuse `Refusing to replace non-symlink at <link>`; untouched | refuse `Refusing to remove non-symlink at <link>`; untouched |

Root guards (both actions, checked before touching the link):

| Condition | Refusal |
| --- | --- |
| prefix is a symlink | `Refusing to operate on symlinked prefix: <prefix>` |
| prefix is not a directory | `Prefix is not a directory: <prefix>` |
| `<prefix>/bin` is a symlink | `Refusing to operate on symlinked bin directory: <bin>` |
| `<prefix>/bin` exists, not a directory | `bin exists but is not a directory: <bin>` |
| `<prefix>/bin` missing | created with `create_dir_all` |

## Hint mapping (`hint_program(argv0)`)
Basename taken first, then one leading login `-` stripped.

| argv0 | result |
| --- | --- |
| `brew` | `brew` |
| `/opt/homebrew/bin/brew` | `brew` |
| `-brew` (login) | `brew` |
| `zapbrew` | `zapbrew` |
| `/usr/local/bin/zapbrew` | `zapbrew` |
| `brew-wrapper` | `zapbrew` |
| `--brew` (only one `-` stripped) | `zapbrew` |
| `` (empty) | `zapbrew` |

## Exact hint output
Three sites now interpolate `ctx.reporter.hint_program()`.

Default reporter (`zapbrew`):
- install/upgrade already-current:
  `<name> <ver> is already installed and up-to-date.\nTo reinstall <ver>, run:\n  zapbrew reinstall <name>`
- services already started:
  `Service `<name>` already started, use zapbrew restart <name> to restart.`

Brew-hint reporter override (proved by `brew_hint_reporter_prefixes_reinstall_and_restart`):
- `opoo:upd 1.0 is already installed and up-to-date.\nTo reinstall 1.0, run:\n  brew reinstall upd`
- `print:Service `svc` already started, use brew restart svc to restart.`

Ruby-mode contrast messages that intentionally name zapbrew were left literal (not routed through the seam).

## Gate results
Command: `cargo fmt --all && cargo test -p zapbrew-ops --test shim --test install --test upgrade --test services && cargo clippy -p zapbrew-ops --all-targets -- -D warnings`

- `cargo fmt --all`: clean.
- tests: PASS - shim 13, install 13, upgrade 7, services 13 (46 total).
- `cargo clippy ... -D warnings`: clean (0 warnings). Test file `.unwrap()` calls were replaced with `.expect(...)` to satisfy `clippy::unwrap-used`.

## Self-review
- No `unsafe`, no `unwrap`, no `TODO`. Symlinks inspected only with `symlink_metadata`/`read_link`; a foreign or dangling link is never followed, overwritten, or deleted (verified by the foreign/dangling install and remove tests plus fingerprint-unchanged assertions).
- Install writes an absolute canonical target; relative existing links are resolved against the link parent before the canonical-identity comparison.
- `remove_file` is used per the brief. It unlinks the symlink entry itself and never its target, so a target swapped after the identity check cannot cause deletion of a foreign file; a swapped-in *link* is the only residual TOCTOU surface, which the brief's `remove_file` contract accepts.
- Scope held to the brief: shim module + lib registration/test seam, the `Reporter::hint_program` default, the three hint sites, `tests/shim.rs`, and shared test-support additions (`RecordingReporter` hint + `context_with_reporter`). No CLI parsing/dispatch changes; Task 7 owns the argv0-derived terminal reporter. Existing focused tests keep asserting the `zapbrew` default through the new seam unchanged.
