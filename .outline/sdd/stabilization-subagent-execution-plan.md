# Stabilization subagent execution plan

## Execution rules

- Use one fresh implementer per task and a fresh reviewer after each implementation.
- Pass context through committed files and task reports, not agent memory.
- Each implementation task writes `.outline/sdd/<task>-report.md` with changed symbols, evidence, and unresolved prerequisites.
- Do not run workspace-wide validation inside parallel tasks. Run targeted proof in the task. Run repository gates once at each atomic commit boundary.
- Keep each commit to one concern. A failed review returns to a fresh fixer before the commit.
- Every commit stages only its named paths. Never use broad staging. `.outline/ledger/` and new `.outline/sdd/` reports are ignored by default and require explicit force-add when the task names them.
- Until J1 commits the journal change, no other task may stage `crates/zapbrew-ops/src/`, `crates/zapbrew-ops/tests/transaction.rs`, or the J0 report.
- Never mark a behavior complete from cross-compilation, command construction, or a fixture when its ledger cell requires a native side effect.
- The ledger, not an agent summary, decides campaign completion.
- L2b schema-v2 sub-task binds `verify.py` tranche-review deltas as one atomic unit: `schema_version`: 2 in `scope.json`, `enums.approval_kinds` includes `tranche-review`, and adversarial checks expand to the full tranche-review and verifier matrix (72 self-tests total).
## Completed architecture leg

### J0: Deepen install-step journal ownership

Status: implemented and reviewed.

Files:

- `crates/zapbrew-ops/src/install_steps.rs`
- `crates/zapbrew-ops/src/transaction.rs`
- `crates/zapbrew-ops/src/postinstall.rs`
- `crates/zapbrew-ops/tests/transaction.rs`
- `.outline/sdd/task-install-step-journal-interface-report.md`

Acceptance:

- `rollback` and `take_cleanup_root` reset journal state.
- Transaction cleanup keeps the `remove_after_commit` seam.
- The step-root cleanup test is mutation-proved.
- `cargo test -p zapbrew-ops` passes.

### J1: Verify and commit the journal concern

Dependencies: J0 reviewed.

Run the repository Rust gates. Commit only the four J0 Rust files and the J0 report as one atomic journal-lifecycle concern. Force-add the ignored report. Verify the commit contains every named file and no ledger, workload, planning, or unrelated user change.

## Ledger bootstrap

### L1: Author canonical scope fragments

Dependencies: J1 committed and Issue 9 final decision.

Run six fresh workers in parallel. Each worker reads one reviewed module inventory and writes one local fragment only:

1. types and prefix;
2. API and network;
3. bottle pour;
4. formula operations;
5. cask, platform, service, tap, and shim;
6. CLI.

Fragment contract:

- stable Module IDs;
- every owned production file;
- canonical behavior/platform/effect cells;
- paired Zapbrew and Homebrew 6.0.16 anchors;
- required evidence kinds;
- references to canonical pending decision IDs;
- no result, review, tranche, performance verdict, or approval state.

Review every fragment with a fresh reviewer. Reject catch-all cells and missing platform mirrors.

### L2: Implement the five-artifact ledger

Dependencies: all L1 fragments reviewed.

One fresh implementer creates:

- `.outline/ledger/scope.json`;
- `.outline/ledger/results.json`;
- `.outline/ledger/approvals.jsonl`;
- `.outline/ledger/tranches.jsonl`;
- `.outline/ledger/verify.py`.

The implementer merges the six fragments, records the exact 84-file bijection, seeds one unresolved result for every cell, and implements every gate from Issue 9. Python uses the standard library only. Exit codes are 0 complete, 1 valid but incomplete, and 2 malformed or contradictory.

Under L2b (schema-v2 tranche-review binding):
- `schema_version` is set to 2 and `enums.approval_kinds` includes `tranche-review`.
- `evidence-pending -> review-pending` transition requires `repository_commit` (40-hex or 64-hex Git commit OID).
- `review-pending -> approved` transition requires `review_ref` naming an authenticated `tranche-review` approval event ID (`approvals.jsonl` event of kind `tranche-review`).
- The `tranche-review` approval object is a fetched GitHub PR review (`https://api.github.com/repos/gosuda/Zapbrew/pulls/{pull_number}/reviews/{review_id}`) authenticated against the checkpoint commit (`repository_commit`) and subject transitive closure digest (`sha256(canonical_bytes(preimage))`).
- Reviewer must be independent (PR author != reviewer) with repository `maintain` or `admin` permission.
- PR base branch must be `main` (`base.ref == "main"`) and `head.sha == repository_commit`.
- Landed snapshot validation verifies that landed checkpoints reach `main` and match their reviewed transitive closure digest; it rejects structural contradictions (exit 2) but permits unrelated globally pending decisions/cells in the committed snapshot, while exact reviewed transitive closure digest equality and live global completion checks remain binding.
- Safety cells still also require their separate `safety-deviation` maintainer approval.

Required adversarial checks (72 total tests):

- remove one file row;
- duplicate one result row;
- omit a macOS native requirement;
- mark a mutation cell complete without failure evidence;
- jump a tranche from building to landed;
- use an unrelated GitHub approval object;
- change a digest-bound proof;
- accept noisy or undersized performance samples;
- mark a hot above-floor unit complete after a no-win revert;
- full tranche-review and verifier matrix cases covering PR base/head mismatch, strict `maintain` or `admin` permission, closure mutation classes (scope, results, deviations, perf_units, workloads, decisions), unrelated tranche+records invariance, retry/replacement ownership, event uniqueness/chronology, and landed snapshot/reachability failures.
Each injection must make the verifier reject the intended defect. Restore the exact files after every injection.
### L3: Review and bind the scope

Dependencies: L2 green.

A fresh reviewer audits scope completeness against all six inventories and the parity report. A repository maintainer then posts the canonical approval statement from Issue 9. Run the online verifier with `GITHUB_TOKEN`; it must authenticate the approval actor and current `maintain` or `admin` permission.

No agent may create or substitute the maintainer approval.

## First Linux behavior tranche

### W1: Implement the deterministic W4 journal workload

Dependencies: L2 verifier available.

One fresh implementer adds an ignored `zapbrew-ops` integration workload. It must:

- generate the bottle and catalog deterministically;
- use `Catalog::from_payload` and loopback transport;
- execute real `install::run` through pour, relocation, receipt, link, and structured overwrite steps;
- ship sentinel bytes at the structured overwrite target so `StepJournal::backup` creates a step-journal root on every measured install;
- prove success by observing the replacement bytes and absence of a step-journal root;
- add an unmeasured deterministic rollback case that observes sentinel restoration and absence of the failed keg and step-journal root;
- call only `install::run` and public APIs that exist at PRE HEAD; never reference `StepJournal` internals or J0-only symbols;
- freeze one fixed iteration count in the source before W2; PRE and POST must run that identical count and the workload must never adapt at runtime;
- use fresh prefix subdirectories while leaving accumulated-tree cleanup outside the timed command through recorded-root hyperfine hooks;
- self-report the install-loop duration and iteration count;
- require no private signing key, public network, or new dependency.

A fresh reviewer verifies that the workload reaches the journal success and cleanup path and does not measure compilation.

### W2: Measure PRE and POST

Dependencies: W1 reviewed.

PRE is commit `dad688adff8b9783ac6c884d1adca01df9861307`, the parent of the journal Interface commit. Use an isolated worktree at that unchanged commit. Copy the exact workload source into PRE and require its digest to equal POST. Before measurement, prove that the crate delta from PRE to POST is exactly the four J0 Rust files and that neither tree has another crate-level change. Never stash or rewrite the active tree.

Build the release test executables into separate target directories with identical `CARGO_PROFILE_RELEASE_DEBUG=true` settings. Record the source, binary, toolchain, platform, architecture, and file-system-stage digests. Smoke the executables directly, then invoke those executables directly for measurement; never place `cargo test` inside the timed command.

For both executables:

- use the same fixed iteration count and invocation;
- keep recorded-root setup and cleanup outside timing with hyperfine prepare and cleanup hooks;
- run `hyperfine --warmup 3 --min-runs 10`;
- store every raw sample and the self-reported install-loop duration;
- reject evidence unless the install loop is at least 90% of hyperfine wall time;
- reject median below one second;
- reject standard deviation at or above 20% of median;
- require `POST median <= PRE median * 1.05`.

Use `perf` on the same debuginfo-enabled executables for at least ten aligned unit-attribution samples with dispersion below 20%. If inlining prevents journal attribution, use a separate stage-disabled build only as a fallback; that third build never supplies PRE/POST gate evidence. Enlarge the fixed workload before both builds if attribution is too short or noisy. A scaling claim needs two fixed declared fixture sizes, one digest per size, and at least ten aligned raw samples with dispersion below 20% for each size.

### W3: Resolve the journal performance branch

Dependencies: W2 trusted.

- Cold: record `fixed` only from W2's validated measured cold classification and a reviewer confirmation that the removed clone and terminal ceremony added no branch, cache, dependency, allocation, or configuration.
- Hot and within twice the measured floor: record `at-floor`.
- Hot and above floor: block, rescope to a journal-only replacement, derive from the contract and floor, and keep only a workload win of at least 1.05x. A failed replacement is reverted and the tranche stays blocked.

Other W4 hot units become later ranked targets. They cannot enter this tranche.

### W4: Record and land Linux journal cells

Dependencies: L3, W2, W3.

Record the five cells from Issue 10: commit success, pre-commit rollback, post-commit cleanup failure, postinstall lifecycle, and per-formula isolation. Homebrew does not expose Zapbrew's journal ownership, inverse replay, or incomplete-cleanup error contract, so all five cells are `safety-deviation-candidate`. Each result requires the complete 13-field deviation proof from Issue 9 and a current repository-maintainer `safety-deviation` approval bound to that exact proof digest. Replay legal tranche events through review. A fresh reviewer verifies every evidence anchor and proof field, and the `review-pending→approved` tranche event references that review. No agent creates or substitutes the maintainer approvals.

The macOS mirror cells remain unresolved. A later native macOS tranche must run file-system process integration; cross-compilation does not complete them.

## Remaining behavior campaign

### P1: Resolve canonical product decisions

Dependencies: L3.

Batch only the decisions that remove or change observable surfaces. Default the rest to the settled constrained-parity policy:

- implement expressible parity by default;
- keep fixed architecture exclusions explicit;
- require repository-maintainer approval for a proved safety deviation;
- ask before removing a live command, flag, stored format, or user data.

Update the one canonical decision record and re-bind the scope digest after each resolution.

### P2: Build atomic parity tranches

Dependencies: P1 for affected cells.

Create one task per coherent behavior cell group, not per source file. Initial order follows the established gap report:

1. accepted-but-inert policy and mirror variables;
2. Linux service start persistence;
3. Linux cask `binary` and approved `appimage` subset;
4. cask variants for info, fetch, state queries, then mutations;
5. cask dry-run and other shared flag contracts;
6. list, doctor, update, services, completions, alias, and output cells.

Each task follows red test or reproduction, implementation, and targeted proof, then remains at `evidence-pending` while P3 supplies performance evidence. After P3, a fresh review, ledger evidence, repository gates, and one atomic commit move the tranche through approval to landed. Each tranche's landing requires an authenticated `tranche-review` of its exact `repository_commit` and transitive closure digest. A `changes-requested` retry invalidates prior approvals and requires a new checkpoint and new `tranche-review` approval. Do not carry a compatibility shim.
### P3: Run all performance targets

Dependencies: L3 and an implemented P2 tranche at `evidence-pending`.

Before a P2 tranche can land, pin the applicable W1 through W4 workload, profile it, work hot units in descending measured share, and compute a measured floor before any replacement. Keep only changes that improve the workload by at least 1.05x and finish every hot unit at no more than twice its floor. Grade every cold unit as fixed, at floor, or left with a concrete cost reason. If later evidence finds a hot above-floor unit in an already-landed tranche, transition that tranche `landed→reverted`, open a new performance-fix tranche ID containing the now-incomplete cells, and repeat implementation, performance, review, and commit gates.

### P4: Complete native platform evidence

Linux systemd-user cells require a live user bus. macOS file-system, launchctl, codesign, hdiutil, ditto, installer, pkgutil, and sw_vers cells require a native macOS host. Use separate native tasks with isolated scratch prefixes and cleanup receipts.

If either host is unavailable, the affected cells remain unresolved and the verifier exits 1. Never replace native evidence with mocks or cross-compilation.

## Final gate

After all reachable tasks:

1. run the ledger verifier online (`verify.py`), requiring every landed owning tranche to carry a current authenticated `tranche-review` binding its exact transitive-closure digest and landed snapshot reachability on `main` (landed snapshot validation rejects structural contradictions but may contain unrelated globally pending decisions/cells; exact reviewed transitive closure and current completion checks remain binding);
2. run `cargo build --workspace`;
3. run `cargo test --workspace`;
4. run clippy with warnings denied;
5. run formatting check;
6. build release;
7. run the live wget install, receipt, uninstall, autoremove, and dangling-link proof;
8. run receipt fixture interoperability;
9. run the required macOS cross-compile tier;
10. run a final independent reviewer and the required alt-reviewer gate.

The campaign is complete only when the ledger exits 0. Missing native hosts, pending product decisions, missing cells, defects, stale approvals, unauthenticated tranche reviews, or hot above-floor units keep it incomplete.
