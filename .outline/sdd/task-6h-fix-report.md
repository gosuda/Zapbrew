# Task 6h audit fix report

## Change
A failed clone now removes the newly created partial tap directory and prunes its empty user directory before returning the command error. The regression runner creates a partial checkout before failing and proves the destination is gone.

## Verification
`cargo fmt -p zapbrew-ops && cargo test -p zapbrew-ops --test tap && cargo clippy -p zapbrew-ops --all-targets -- -D warnings && cargo fmt -p zapbrew-ops -- --check && cargo test -p zapbrew-ops`

Result: exit 0. Tap tests: 4 passed. Full zapbrew-ops tests: 188 passed. Formatting and clippy: clean.
