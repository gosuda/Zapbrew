# Task 6i second audit fix report

## Change
Systemd environment serialization now doubles literal `%` before escaping backslashes and quotes. The exact unit snapshot covers `PERCENT=100%` and expects `PERCENT=100%%`.

## Verification
`cargo fmt --all && cargo test -p zapbrew-ops --test services && cargo clippy -p zapbrew-api -p zapbrew-ops --all-targets -- -D warnings && cargo fmt --all -- --check && cargo test -p zapbrew-api && cargo test -p zapbrew-ops`

Result: exit 0. Services: 11 passed. API: 51 passed. Ops: 205 passed. Formatting and clippy: clean.
