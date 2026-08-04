# Task 6i fourth audit fix report

## Change
The valid API shape `keep_alive: {"always": false}` now parses as disabled keep-alive. Systemd emits no `Restart=` row and launchd emits no `KeepAlive` key.

## Verification
`cargo fmt --all && cargo test -p zapbrew-ops --test services && cargo clippy -p zapbrew-api -p zapbrew-ops --all-targets -- -D warnings && cargo fmt --all -- --check && cargo test -p zapbrew-api && cargo test -p zapbrew-ops`

Result: exit 0. Services: 13 passed. API: 51 passed. Ops: 207 passed. Formatting and clippy: clean.
