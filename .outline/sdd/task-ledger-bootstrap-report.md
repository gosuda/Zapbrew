# Task: Ledger bootstrap report

## Artifacts produced

`.outline/ledger/` contains the five authoritative artifacts required by the stabilization-ledger design:

- `scope.json` — policy, 84 file rows, canonical cells, and decisions D2-D8
- `results.json` — one unresolved result per cell, empty deviations/workloads/perf_units
- `approvals.jsonl` — append-only, empty
- `tranches.jsonl` — append-only, empty
- `verify.py` — Python 3.14.6, standard library only, `argv` supports `--root`, `--offline`, `--self-test`

No Rust or other repository source files were edited. All five artifacts live under the ignored `.outline/ledger/` directory.

## Scope counts

| Item | Count |
|------|-------|
| Production Rust files | 84 |
| Canonical cells | 446 |
| Unresolved results | 446 |
| Pending decisions | 7 (D2-D8) |
| Empty approval events | 0 |
| Empty tranche events | 0 |

The 84 file rows are sorted by `path` and are an exact bijection with `crates/*/src/**/*.rs` on disk. The ledger must contain exactly 84 source files and exactly 446 canonical cells; removing a file row, a cell, or its matching result without the other is an integrity error (exit 2). Cell IDs and result IDs are each unique and in bijection. Platform `any/linux/macos` mirrors are present where the fragment authors supplied them.

## Schema

- `schema_version`: 2
- `homebrew_target`: `6.0.16@3ecc9eff23feebf1bc73846d74e14a122c93b66f`
- `precedence`: `constrained-parity`
- `authority`: `repository-maintainer`
- `repository`: `gosuda/Zapbrew`
- `decisions[]`: D2-D8, all `status: pending`
- `enums`: closed sets for platforms, effects, dispositions, evidence kinds, findings, completed dispositions, approval kinds (including `tranche-review`), tranche states/edges, mutation/native effects, and native proof kinds
- Event ID uniqueness: event IDs are globally unique across `approvals.jsonl` and `tranches.jsonl` (duplicate event ID is exit 2).
- Per-tranche event chronology: `event_time` is strictly monotonically increasing per tranche (`last_time < event_time`); equal or non-monotonic times are exit 2.
- Transition requirements: `evidence-pending -> review-pending` requires `repository_commit` (40-hex or 64-hex Git commit OID); `review-pending -> approved` requires `review_ref` naming an authenticated `tranche-review` approval event ID.
- Online review binding: `review_ref` sets `reviewed` only after online authentication of the GitHub PR review object; offline or tokenless runs leave tranches unreviewed (exit 1), while structural invalidity remains exit 2.
## Corrected verification gates

`verify.py` now enforces the following corrected bootstrap and completion gates.

Stale approval events are historical and non-blocking. A newer current authenticated approval event for the same `(kind, subject_id)` restores approval. Fetched actor/body/body-digest/canonical-line/permission failures are unresolved candidates, reported only when no valid current event approves the subject. Static malformed JSON/event schema, wrong repository, wrong Homebrew commit, unknown subject, and impossible chronology remain integrity errors (exit 2). Offline and missing `GITHUB_TOKEN` behavior remain exit 1.

Native proof platform binding is enforced. For `linux` or `macos` cells, achieved evidence of kind `process-integration` or `native-side-effect` must carry the exact cell platform. Cross-compile, command construction, unit fixture I/O, and structural parse evidence do not satisfy a native side-effect on the wrong platform. Platform-`any` mutation cells additionally require `cross-compile` in their expected evidence and native `process-integration` or `native-side-effect` proof achieved on both `linux` and `macos`; a single host or `platform=any` evidence leaves the cell incomplete.

Every `product-decision-pending` scope cell must have at least one `decision_refs` entry. A cell with no controlling decision is structurally invalid.

Performance workload/cell platform compatibility is enforced. A `linux` or `macos` workload may attribute to `any` or same-platform cells only. Cross-platform performance attribution (for example, a Linux workload to a macOS cell) is an integrity error.

Trusted sample dispersion is `statistics.stdev(samples) < 0.20 * statistics.median(samples)`. Equality or a larger value is rejected as noisy (exit 2). This replaces the prior mean-based denominator and rejects right-skewed samples such as `[1.0]*8 + [1.385, 1.586]`, which the old rule accepted because their standard deviation was below 20% of the inflated mean while it exceeds 20% of the median.

A hot-reclassification approval may reclassify a hot performance unit. Otherwise a hot performance unit requires an authenticated `performance-floor` approval for that exact unit before the floor multiple can complete it; missing floor approval is exit 1. A `unit_median/floor` greater than 2 remains incomplete if the unit is not approved (reclassification or floor). The no-win-revert block is preserved: a `revert` action whose `pre_median/post_median` is below 1.05x also remains incomplete.

A `review_ref` is permitted only on transitions out of `review-pending` and must name an authenticated `tranche-review` approval event ID. Only a `review-pending -> approved` transition with a valid, authenticated `review_ref` sets `reviewed`; numeric, boolean, early, or unauthenticated `review_ref` values cannot confer review. Structurally invalid or mismatched `review_ref` values are integrity errors (exit 2), while missing credentials or offline evaluation leave tranches unreviewed (exit 1). A reverted tranche is valid terminal history and does not produce a tranche-level `not landed` blocker; its cells remain incomplete under cell-level active-owner/landed checks until a replacement reviewed-landed tranche takes ownership. Hot-reclassification approvals bind to the complete referenced workload record (not just the workload id), so any canonical workload mutation (including samples) stales the approval.

Tranche review authentication enforces a canonical transitive-closure subject digest and strict GitHub PR review verification. The subject preimage contains sorted `tranche_id`, `checkpoint_event_id`, `repository_commit`, member `cells` (sorted full records), `decisions` (sorted full records for cell `decision_refs` + checkpoint `decision_refs`), `deviations` (sorted full records for non-null member results), `perf_units` (sorted full records for selected workload perf units + checkpoint `workload_refs`), `workloads` (sorted full records), and `scope_cells` (sorted full records). Digest calculation is `sha256(canonical_bytes(preimage))` with explicitly sorted keys and UTF-8 encoding. The fetched review body must contain `ZAPBREW-TRANCHE-REVIEW v1 tranche=<tranche_id> commit=<repository_commit> review_ref=<checkpoint_event_id> digest=<subject_digest>`. Authentication verifies `parse_review_url` (`https://api.github.com/repos/gosuda/Zapbrew/pulls/{pull_number}/reviews/{review_id}`), PR default-branch reachability (`base.ref == "main"`), committed-ledger snapshot equality (`head.sha == repository_commit`), reviewer independence (`pr_obj.user.login != review.user.login`), review `APPROVED` state, `commit_id == repository_commit`, `submitted_at >= checkpoint_time`, `created_at <= approval_event_time`, and maintainer permission (`maintain` or `admin`). Landed snapshot validation verifies that landed checkpoints reach `main` and match their reviewed transitive closure digest; it rejects structural contradictions (exit 2 for malformed scope, results, or tranches in the committed snapshot) but permits unrelated globally pending decisions/cells in the committed snapshot, while exact reviewed transitive closure digest equality and live global completion checks remain binding.

Tranche events carrying optional `decision_refs` or `workload_refs` must reference them as non-empty arrays of unique non-empty strings, and every ID must resolve to a known decision or workload record. An optional `reason` must be a non-empty string, and an optional `repository_commit` must be one lowercase full Git object ID of exactly 40 or 64 hexadecimal characters. A `proposed -> scoped` transition must carry `cells` as a non-empty array of unique non-empty strings, each resolving to a canonical scope cell; malformed or unknown values are rejected as exit-2 integrity errors before the transition is applied.

Homebrew source anchors are pinned for both `must-match` and `safety-deviation-candidate` cells: the only valid `homebrew` URLs start with the exact `6.0.16` tag or the exact `3ecc9eff23feebf1bc73846d74e14a122c93b66f` commit path. `must-be-outside-architecture` explanatory comparison strings remain exempt from this pin.

Approval self-tests cover stale, current, and unrelated events. Unrelated and stale historical approvals no longer produce a permanent exit 2. The self-test set includes a stale-old-plus-current-new reapproval case and a wrong-platform native-proof case. Tranche ownership self-tests cover reverted tranche release: a `review-pending -> reverted` transition removes each member cell from `active_owner` so a later `proposed -> scoped` transition for the same cell does not produce an ownership-integrity error, and the verifier permits the reverted tranche to be replaced by a reviewed landed tranche.

`verify.py` uses only the Python 3.14 standard library (`argparse`, `ast`, `hashlib`, `json`, `pathlib`, `statistics`, `tempfile`, `urllib`, etc.). It derives all coverage, tranche, review, benchmark, and completion state from raw records.

## Self-test set

The verifier includes an adversarial self-test that copies the ledger into a temporary directory and mutates only the copy. The current set is 72 tests:

- **Baseline & Scope Integrity (14 tests)**: `missing file row -> exit 2`, `placeholder Homebrew anchor -> exit 2`, `duplicate result -> exit 2`, `matched cell and result shrinkage -> exit 2`, `duplicate scoped cells -> exit 2`, `empty scoped cells -> exit 2`, `missing macOS mutation tier -> exit 2`, `mutation complete without failure -> exit 1`, `building to landed jump -> exit 2`, `malformed repository commit -> exit 2`, `duplicate decision refs -> exit 2`, `non-string decision ref -> exit 2`, `product-decision-pending cell with no decision ref -> exit 2`, `unpinned safety-deviation anchor -> exit 2`.
- **Approvals & Deviation Integrity (6 tests)**: `unrelated/malformed approval body -> exit 1`, `stale historical approval no longer exit 2 -> exit 1`, `stale old plus current new reapproval -> exit 1`, `stale deviation proof digest -> exit 2`, `Linux native proof on macOS cell -> exit 2`, `platform-any single-host native proof stays incomplete -> exit 1`.
- **Performance & Floor Gates (10 tests)**: `perf unit cross-platform attribution -> exit 2`, `undersized performance samples -> exit 2`, `noisy performance samples -> exit 2`, `right-skewed median dispersion -> exit 2`, `hot above-floor no-approval -> exit 1`, `inflated floor without approval -> exit 1`, `current authenticated floor approval removes only missing-floor marker`, `hot above-floor reclassified by authenticated approval -> note`, `workload mutation stales hot reclassification approval -> exit 1`, `hot above-floor no-win revert -> exit 1`.
- **Tranche Review Offline & Auth Validation (14 tests)**: `valid tranche review offline remains unreviewed -> exit 1`, `tranche review fetch unavailable remains unreviewed -> exit 1`, `noncanonical tranche-review URL -> exit 2`, `review object ID mismatch -> exit 1`, `review pull-request URL mismatch -> exit 1`, `review actor mismatch -> exit 1`, `pull-request author self-review -> exit 1`, `reviewer permission insufficient -> exit 1` (strict `maintain` or `admin` permission requirement), `pull-request base mismatch -> exit 1` (`base.ref == "main"`), `pull-request head mismatch -> exit 1` (`head.sha == repository_commit`), `review state not APPROVED -> exit 1`, `review commit mismatch -> exit 1`, `review body is not canonical -> exit 1`, `review body digest mismatch -> exit 1`.
- **Tranche Review References & Event Chronology (11 tests)**: `review submitted before checkpoint -> exit 2`, `approval event precedes submitted review -> exit 2`, `early or malformed review ref cannot confer review -> exit 2`, `unknown workload ref -> exit 2`, `legacy forged review_ref names no approval -> exit 2`, `review checkpoint without repository commit -> exit 2`, `review_ref names non-review approval -> exit 2`, `review_ref names another tranche review -> exit 2`, `review_ref names late review event -> exit 2`, `non-monotonic tranche event time -> exit 2`, `duplicate tranche event ID -> exit 2`.
- **Transitive Closure Mutation Classes (6 tests)**: `scope closure mutation stales tranche review -> exit 1`, `result closure mutation stales tranche review -> exit 1`, `deviations closure mutation stales tranche review -> exit 1`, `perf_units closure mutation stales tranche review -> exit 1`, `workloads closure mutation stales tranche review -> exit 1`, `decisions closure mutation stales tranche review -> exit 1`.
- **Invariance & Event Stream Integrity (2 tests)**: `unrelated tranche append preserves current review` (unrelated tranche+records invariance), `empty logs bootstrap remains exit 1`.
- **Landed Snapshot & Reachability Failures (8 tests)**: `landed checkpoint unreachable from default branch -> exit 2`, `landed repository fetch unavailable -> exit 1`, `landed compare fetch unavailable -> exit 1`, `landed comparison missing status -> exit 1`, `landed repository missing default branch -> exit 2`, `landed committed file malformed base64 -> exit 2`, `landed committed file malformed JSON -> exit 2`, `exact reachable committed snapshot has no landed-review error`.
- **Retry & Replacement Ownership (1 test)**: `reverted tranche replaced by reviewed landed tranche -> exit 1` (verifies ownership release on revert and replacement by a reviewed landed tranche).
The exit semantics are unchanged: 2 for malformed or contradictory ledgers, 1 for structurally valid but incomplete ledgers, and 0 for global completion.

## Unresolved blockers

The ledger is intentionally incomplete at bootstrap. The recorded blockers are:

- No current repository-maintainer scope approval (scope approval is the first binding gate in L3).
- All seven decision records D2-D8 are `pending`.
- All 446 canonical cells have empty `achieved_evidence` and `failure_evidence`.
- `approvals.jsonl` and `tranches.jsonl` are empty.
- No workloads, performance samples, or deviation proofs exist.
- macOS native side-effect cells require a native macOS host; this Linux workstation cannot complete them.

Because these missing pieces are the intended bootstrap state, the verifier reports a structurally valid but incomplete ledger. No false completion, placeholder, fake approval, or third-party import was introduced.
