# Install-step journal Interface report

## Scope

This change deepens the journal for one formula's reversible install-step file-system effects. It does not change formula or keg selection, the outer install transaction, command execution, error attribution, or cross-formula behavior.

## Interface

`StepJournal` keeps two terminal operations:

- `rollback(&mut self, env) -> Vec<Utf8PathBuf>` applies inverse operations in reverse order, attempts to remove the journal root, returns sorted unique leftovers, and resets all journal fields.
- `take_cleanup_root(&mut self) -> Option<Utf8PathBuf>` transfers the journal-root path to the caller without a clone and resets all journal fields.

`cleanup_path` and `clear` are removed. The transaction and postinstall callers no longer coordinate a borrowed path with a separate clear operation.

## Contracts

### Rollback

- PRE: Install-step execution failed before commit.
- POST: Each recorded inverse was attempted in reverse order. The journal root was removed when possible. All surviving paths are returned. The journal is empty.
- INVARIANT: The outer transaction runs this rollback before skeleton, link, promoted-keg, replacement-backup, old-keg, stage, and created-directory rollback.

### Cleanup-root transfer

- PRE: Install-step execution succeeded and the caller committed the keg.
- POST: The caller owns the optional root path. The journal is empty.
- INVARIANT: The caller still owns physical removal and error context. `FormulaTransaction` uses `remove_after_commit`, which preserves failure injection and cleanup batching. `postinstall` uses `remove_tree_confined` and attributes leftovers to its selected keg.

### Boundary

The journal records reversible file-system effects only. `Run` and maintenance command effects remain outside journal compensation. Each formula owns a separate journal.

## Allocation and control-flow effect

The transaction success path no longer clones the journal-root path. The change adds no branch, scan, command, or heap allocation. It removes the `cleanup_path` plus `clear` call sequence from both callers.

## Regression proof

The new transaction test installs a new formula whose structured step overwrites an existing confined file. It arms the one-shot committed-cleanup failure hook. The test verifies:

- `CleanupIncomplete` reports the step-journal root;
- the committed keg and all links remain active;
- the committed file contains the new bytes;
- exactly one `.zapbrew-step-journal-*` directory survives.

Targeted green command:

```text
cargo test -p zapbrew-ops --test transaction step_root_cleanup_failure_keeps_new_keg_active -- --exact
1 passed; 0 failed
```

Mutation proof: replacing `remove_after_commit` with `remove_tree_confined` for the step root made the test fail at `step journal cleanup is reported`. Restoring the source restored the original SHA-256, `87c0cc2fa76de637795b46b5f015c3a5798f24633f11e540595ca57286b1fc1a`, and the targeted test passed again.
