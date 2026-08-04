# Task 6g final audit fix report

## Change
- Validated the configured Cellar with the existing no-follow root check.
- Added an invalid Cellar to the sorted configured-root finding.
- Skipped unlinked-keg discovery when the Cellar root is invalid.
- Added a symlink regression that proves the external target is not named or changed.

## Focused verification
`cargo fmt -p zapbrew-ops && cargo test -p zapbrew-ops --test doctor && cargo clippy -p zapbrew-ops --all-targets -- -D warnings`

Result: exit 0. Doctor tests: 9 passed. Clippy: clean.
