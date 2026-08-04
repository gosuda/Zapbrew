# Task 6g residual fix report

## Decisions

| Case | Result |
|---|---|
| Direct alias to a real cache file | Keep alias and target |
| Alias through an escaping cache symlink | Remove cache aliases; never inspect or change external target |
| Exact latest or installed bottle version | Keep |
| Prefix-colliding bottle version | Remove under `--scrub` |
| Linked symlink to the matching Cellar keg | Treat as linked |
| Linked symlink outside the Cellar | Report rack as unlinked; never name or change target |

## Verification
`cargo fmt -p zapbrew-ops && cargo test -p zapbrew-ops --test cleanup --test doctor && cargo clippy -p zapbrew-ops --all-targets -- -D warnings`

Result: exit 0. Cleanup and doctor tests: 28 passed across two suites. Clippy: clean.
