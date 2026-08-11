# Linux service registration report

## Scope

This tranche implements decision `D3-linux-service-registration` for Linux systemd user services.

It changes these cells:

- `cp.services.linux-start-native`: `start` enables the selected unit before it starts the unit.
- `cp.services.linux-restart-native`: `restart` keeps the prior enabled or unenabled mode.
- `cp.services.linux-run-native`: `run` remains unenabled.

PRE is `61b6dd2`. POST is the uncommitted Linux registration change.

This tranche does not change macOS behavior, stop behavior, file removal, `daemon-reload` behavior outside the start registration sequence, a future `--keep` option, or Linux rollback behavior.

## Contract

The selected unit is `homebrew.<name>.timer` for a timed service. It is `homebrew.<name>.service` otherwise.

An inactive `start` uses this order:

```text
systemctl --user is-active <unit>
write service and optional timer files
systemctl --user daemon-reload
systemctl --user enable <unit>
systemctl --user start <unit>
```

An active and enabled `start` uses this order and keeps the prior no-op message:

```text
systemctl --user is-active <unit>
systemctl --user is-enabled <unit>
```

An active but unenabled `start` converts the registration without a second start:

```text
systemctl --user is-active <unit>
systemctl --user is-enabled <unit>
write service and optional timer files
systemctl --user daemon-reload
systemctl --user enable <unit>
```

`restart` checks `is-enabled` before `stop` when the unit file exists. An enabled unit returns through the persistent write, enable, and start path. An unenabled unit returns through the transient run path. A missing unit skips `is-enabled` and defaults to the persistent path.

`run` does not call `enable`.

Homebrew 6.0.16 starts and then enables. D3 intentionally requires enable and then start. The D3 decision governs this constrained-parity difference.

## Command evidence

Official systemd 261.2 documentation states that `enable` creates the `[Install]` links and does not start the unit. It states that `is-enabled` reports the unit-file enablement state:

- https://www.freedesktop.org/software/systemd/man/latest/systemctl.html#enable%20UNIT%E2%80%A6
- https://www.freedesktop.org/software/systemd/man/latest/systemctl.html#is-enabled%20UNIT%E2%80%A6

The workstation has systemd 259. A real read-only argv probe accepted the constructed command and returned the expected missing-unit state:

```text
$ systemctl --user is-enabled zapbrew-argv-probe.service
not-found
exit 4
```

This probe proves argument acceptance. It does not prove enable or restart side effects.

## Verification

```text
$ cargo fmt --all -- --check
pass

$ cargo test -p zapbrew-ops --test platform
6 passed; 0 failed

$ cargo test -p zapbrew-ops --test services
45 passed; 0 failed

$ cargo clippy -p zapbrew-ops --all-targets -- -D warnings
pass

$ cargo test -p zapbrew-ops
412 passed; 0 failed; 2 ignored
```

The standard reviewer returned PASS with confidence 0.98. The mandatory alternate reviewer returned PASS with confidence 0.96.

## Evidence boundary

The tests prove command construction, order, state routing, output, unit selection, and error propagation through the injected command boundary.

The Linux native-side-effect cells still need a live user D-Bus proof for enablement and restart persistence. That proof belongs to the native-platform evidence task. This report does not claim authenticated scope approval, safety approval, tranche approval, or landing on `main`.
