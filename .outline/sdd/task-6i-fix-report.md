# Task 6i audit fix report

## Change
- launchd plist serialization now includes the substituted `StandardErrorPath`.
- systemd command quoting doubles literal `%` so systemd does not treat user arguments as specifiers.
- exact render tests cover both cases.

## Verification
`cargo fmt --all && cargo test -p zapbrew-ops --test services && cargo clippy -p zapbrew-api -p zapbrew-ops --all-targets -- -D warnings && cargo fmt --all -- --check && cargo test -p zapbrew-api && cargo test -p zapbrew-ops`

Result: exit 0. Services: 11 passed. API: 51 passed. Ops: 205 passed. Formatting and clippy: clean.
