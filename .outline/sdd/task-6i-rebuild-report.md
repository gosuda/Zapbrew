# Task 6i typed service rebuild report

## Change

Rebuilt the two ad hoc resolution paths in `crates/zapbrew-ops/src/services.rs` into typed,
total models, per `agent://PlanTypedServicesRebuild` and the two conceded
`agent://DefendServicesContract` findings only.

- **String `run` → `/bin/sh -c`.** `parse_run` now resolves a string command to
  `["/bin/sh", "-c", <substituted>]` instead of a single unsplittable argv token. Substitution
  still runs before wrapping, so leftover-placeholder refusals fire unchanged. Array/object/other/
  missing branches are untouched.
- **Typed cron.** `struct Cron` now holds `CronField` per field (`Any` | `Set(Vec<u32>)`, sorted,
  deduped, non-empty, bounds-checked). `parse_cron_field`/`parse_cron_term` accept `*`, single
  integers, comma lists, inclusive ranges, and `/step` on wildcard/range/single terms, rejecting
  zero steps, descending ranges, empty terms, malformed numbers, bare `*` inside a list, and
  out-of-range values with the existing single `invalid service cron schedule` refusal. Bounds:
  minute 0–59, hour 0–23, day 1–31, month 1–12, weekday 0–7.
- **Expansion bound.** `cron_combinations` computes the checked product of non-`Any` set lengths;
  `parse_cron` refuses any schedule exceeding `MAX_CRON_COMBINATIONS = 4096` (or overflowing) with a
  named `expands too broadly` refusal. This runs at the parse boundary, before `ServiceConfig` is
  built and before any Cartesian allocation, so the refusal precedes all file/command I/O.
- **Renderers.** `systemd_calendar` emits native comma-lists (minute/hour padded, day/month
  unpadded, weekday deduped by name). `plist_calendar` returns a single `Dictionary` when every
  active field is single-valued (byte-identical to prior output) or a Cartesian `Array` of
  dictionaries otherwise (Minute…Weekday order, last field varying fastest, weekday kept raw).
- **Deletions.** Removed `pad_cron`, the bare-`u32` cron validation, and the dead
  `plist_calendar` `u64` re-parse plus its `OpError::InvalidState` "stopped parsing" branch.

Preserved verbatim: `ServiceConfig`, environment sorting, substitution allowlist, path confinement,
systemd percent escaping (`systemd_quote`/`systemd_escape`), launchd scaffold, keep-alive semantics,
plist serializer, injected `CommandRunner`/`Reporter`, and list/start/stop/restart controller flow.
`update.rs` and `lib.rs` unchanged.

## Normalized cron outputs

| cron | systemd `OnCalendar` | launchd `StartCalendarInterval` |
|---|---|---|
| `5 3 * * 1` (existing) | `Mon *-*-* 03:05:00` | dict `{Minute:5,Hour:3,Weekday:1}` |
| `*/15 * * * *` | `*-*-* *:00,15,30,45:00` | array `[{Minute:0},{Minute:15},{Minute:30},{Minute:45}]` |
| `30 2 1,15 * *` | `*-*-1,15 02:30:00` | array `[{Minute:30,Hour:2,Day:1},{Minute:30,Hour:2,Day:15}]` |
| `0 12 * * 1-5` | `Mon,Tue,Wed,Thu,Fri *-*-* 12:00:00` | array `[{Minute:0,Hour:12,Weekday:1}…{…Weekday:5}]` (5) |
| `0 0 * * 0,7` | `Sun *-*-* 00:00:00` (deduped by name) | array `[{Minute:0,Hour:0,Weekday:0},{…,Weekday:7}]` (raw) |

## Exact snapshots

### String run — systemd unit (`run: "$HOMEBREW_PREFIX/bin/demo serve --flag"`)

```
[Unit]
Description=Homebrew generated unit for demo

[Install]
WantedBy=default.target

[Service]
Type=simple
ExecStart="/bin/sh" "-c" "$ROOT/prefix/bin/demo serve --flag"
```

### String run — launchd `ProgramArguments`

```
/bin/sh
-c
$ROOT/prefix/bin/demo serve --flag
```

### `*/15 * * * *` — launchd `StartCalendarInterval`

```xml
<key>StartCalendarInterval</key>
<array>
	<dict><key>Minute</key><integer>0</integer></dict>
	<dict><key>Minute</key><integer>15</integer></dict>
	<dict><key>Minute</key><integer>30</integer></dict>
	<dict><key>Minute</key><integer>45</integer></dict>
</array>
```

### `30 2 1,15 * *` — launchd `StartCalendarInterval`

```xml
<key>StartCalendarInterval</key>
<array>
	<dict><key>Minute</key><integer>30</integer><key>Hour</key><integer>2</integer><key>Day</key><integer>1</integer></dict>
	<dict><key>Minute</key><integer>30</integer><key>Hour</key><integer>2</integer><key>Day</key><integer>15</integer></dict>
</array>
```

`0 12 * * 1-5` renders five weekday dictionaries (`Weekday` 1..5); `0 0 * * 0,7` renders two
(`Weekday` 0 and 7) while systemd dedupes both to `Sun`.

## Mutation proof

Both conceded regressions were mutation-verified: fix reverted → the guarding test fails → fix
restored → suite green.

| regression | mutation | result |
|---|---|---|
| string run `/bin/sh -c` | revert `parse_run` string arm to `vec![substitute(...)?]` | `string_run_wraps_command_in_sh_c_for_systemd_and_launchd` FAILED (`ExecStart="$ROOT/prefix/bin/demo serve --flag"`, one token); restored → pass |
| cron `*/N` step | `parse_cron_term` returns `None` for any `/step` term | `cron_step_field_renders_native_systemd_list_and_launchd_array` FAILED (`invalid service cron schedule` refusal); restored → pass |

## Gate results

`cargo fmt --all` + `cargo fmt --all -- --check` + `cargo test -p zapbrew-ops --test services`
+ `cargo test -p zapbrew-ops` + `cargo clippy -p zapbrew-ops --all-targets -- -D warnings` — all exit 0.

- Focused services: **19 passed** (13 pre-existing + 6 new: sh-c, cron step, cron list/range,
  Sunday 0/7, malformed-refuse-before-I/O, broad-Cartesian-refuse-before-I/O).
- Full `zapbrew-ops`: all suites pass (services 19; every other suite green), 0 doctests.
- Clippy: clean under `-D warnings`, no new `#[allow]` in production code.

## Self-review

- **Contract preserved.** No `process_type`/`require_root`, no `Restart=always`, no keep-alive
  changes, no broadened placeholder interpolation — only the two conceded findings implemented.
- **Failure precedes mutation.** Cron/run refusals are raised inside `target`→`parse_service`,
  which runs before `start` writes any file or invokes any command; both refusal tests drive the
  full `services::run` controller under `PanicRunner` and assert no service/timer file exists and
  no output was emitted.
- **Illegal states unrepresentable.** `CronField::Set` only exists post-validation (sorted, deduped,
  non-empty, in-range), removing the fallible render-time re-parse and its dead `InvalidState`
  branch; renderers are total over `CronField`.
- **No unsafe / no unwrap / no TODO / no lint suppression.** Production uses `checked_add`/
  `checked_mul` and `.ok_or_else`; tests use `.expect` (clippy `unwrap_used=deny` respected).
- **Snapshots stable.** All pre-existing exact systemd/launchd/keep-alive/controller snapshots are
  unchanged; only the two required behavioral differences appear, both in new tests.
