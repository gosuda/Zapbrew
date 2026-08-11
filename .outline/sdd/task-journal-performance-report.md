# Journal performance evidence

## Scope

This report binds the W1 workload, W2 measurements, and W3 classification for the install-step journal ownership change. It does not approve scope, safety deviations, or a tranche. Those gates require authenticated repository-maintainer actions.

PRE is `dad688adff8b9783ac6c884d1adca01df9861307`. POST contains the journal terminal change. Both builds use the same workload source, release profile, debug information, toolchain, fixture, and file-system stage.

## Receipts

### Workload source

- Path: `crates/zapbrew-ops/tests/w4_install_journal.rs`
- SHA-256: `a9746d8da580870dc2509739d60bfe940fed80f1d50309634dd6b83efde6278b`
- Iterations: `2048`
- Timed region: sequential `install::run` calls only
- Setup, assertions, and cleanup: outside the self-reported install-loop interval

### Fixture

Canonical JSON preimage:

```json
{"arch":"x86_64","bottle_entries":[{"bytes":"w4 executable\\n","mode":"0755","path":"w4-journal/1.0/bin/w4-journal"},{"bytes":"prefix=@@HOMEBREW_PREFIX@@\\n","mode":"0644","path":"w4-journal/1.0/share/relocated.txt"}],"formula":"w4-journal","iterations":2048,"parallelism":2,"platform":"linux","sentinel_bytes":"sentinel-before-install\\n","structured_step":{"base":"var","content":"replacement-from-install\\n","overwrite":true,"path":"w4-journal/managed.conf","type":"write"},"timed_region":"2048 sequential install::run calls","transport":"loopback GET /w4-journal","version":"1.0"}
```

SHA-256: `5f4c0d16e3e3f2180df48cd21b0c27b5c450dffb7aac82b0b2cfc3623af75967`

### File-system stage

Canonical JSON preimage:

```json
{"cargo_excluded":true,"cleanup":"isolated rip permanent delete of recorded_root","filesystem":"tmpfs","prepare":"isolated rip permanent delete of recorded_root","recorded_root":"/tmp/zapbrew-w4-install-journal-roots","timed_command":"direct release test executable"}
```

SHA-256: `6ae441cf65b1587834b54719462a98999b3498b79dd6eba40ff652e59bfd97f0`

### Toolchain

Canonical JSON preimage:

```json
{"commit":"8bab26f4f68e0e26f0bb7960be334d5b520ea452","commit_date":"2026-07-14","debug":true,"host":"x86_64-unknown-linux-gnu","llvm":"22.1.6","profile":"release","rustc":"1.97.1"}
```

SHA-256: `ce1e068fdb6f1a46e17b4ccf7781576f64e0e01d43b67fb889e6ef454b621466`

## W2 measurements

Each run used three warmups and ten measured samples. Each wall-time median exceeds one second. Each sample set has standard deviation below 20% of its median. The median install-loop share exceeds 90% for PRE, POST, and the attribution probe.

### PRE

- Executable SHA-256: `fb76482c155fdc36b9eb5c535c8efa66d5e0fc3ee54f5e1c1767bfb0afc25845`
- Raw sample artifact SHA-256: `efdc2ac6f7964f6eda5715919207fefb2596bfcc82d2c03ca683e3ad245cd957`
- Wall samples, seconds: `[2.43072725034, 2.2718253433399997, 2.34648681234, 2.53004498334, 2.32520338434, 2.4501854863399997, 2.27870491434, 2.3327367523399998, 2.3286074153399996, 2.37348861534]`
- Install-loop samples, seconds: `[2.224904596, 2.070375873, 2.129024939, 2.293036234, 2.137501049, 2.217971704, 2.072694856, 2.117467471, 2.129665941, 2.166108936]`
- Wall median: `2.3396117823399996`
- Wall standard deviation: `0.08115575336186705`
- Install-loop median: `2.133583495`
- Install-loop standard deviation: `0.07070175332731961`
- Median loop share: `0.9119391136191248`

### POST

- Executable SHA-256: `a54a167cb4d37dd55e9c56db581edb52c5cbd9e23bc7e309a22fdccc5abcc5a5`
- Raw sample artifact SHA-256: `3da86320425a71d9755af33501d397f3069e0128e58a021757469bd052185adf`
- Wall samples, seconds: `[2.3517291685, 2.3825219135, 2.3403264445, 2.3443018245, 2.3209546145, 2.2764398345, 2.2891588805, 2.2653996165, 2.3289225965, 2.3703589905]`
- Install-loop samples, seconds: `[2.149720609, 2.186298734, 2.157734430, 2.144824213, 2.124446649, 2.078456356, 2.085795102, 2.0691242, 2.090616491, 2.168064438]`
- Wall median: `2.3346245205000002`
- Wall standard deviation: `0.03926306383946764`
- Install-loop median: `2.134635431`
- Install-loop standard deviation: `0.0417573962902833`
- Median loop share: `0.9143377927611378`

POST/PRE wall median ratio is `0.9978683378679982`. POST satisfies `POST <= PRE * 1.05`. This is a non-regression result, not a speedup claim.

### Journal-disabled attribution probe

The probe keeps path resolution, the overwrite guard, template expansion, existing-target removal, parent creation, and `fs::write`. It replaces only `StepJournal::replace_before_mutation` with direct existing-target removal. The workload still performs the structured overwrite, but it creates no journal backup, inverse, root, or terminal root cleanup.

- Probe source SHA-256: `165e1dffb15ee11733a13e7d7f83961b4810d36ffeee42a75d14e03025457040`
- Exact probe delta digest: `f2f7699a5e89c46bffa6185232d374217f509898f43bbcaaa1e40f44c3a441b5`
- Executable SHA-256: `99c278eed69d2697c3bc9620de1d977179f29aaa2c2a953c7d8e1597122d4bd0`
- Raw sample artifact SHA-256: `4e021b6cb45d707246b2f88e1f6935a395f9ba7bd072f23216442d94d27b1a21`
- Wall samples, seconds: `[2.35035610108, 2.37502221508, 2.41695446008, 2.2154510800800002, 2.3658144880800003, 2.23467268708, 2.18987075808, 2.35298838808, 2.55746996408, 2.38687078908]`
- Install-loop samples, seconds: `[2.12225723, 2.168630387, 2.210812154, 2.02805898, 2.167526401, 2.037557769, 1.996750292, 2.147422651, 2.328687089, 2.177673367]`
- Wall median: `2.35940143808`
- Wall standard deviation: `0.10879095167412635`
- Install-loop median: `2.1574745259999997`
- Install-loop standard deviation: `0.09860587148889621`
- Median loop share: `0.9144160426365081`

The signed wall-median contrast is `POST - probe = -0.02477691757999967` seconds. The POST/probe wall ratio is `0.9894986426726253`. Disabling the journal did not improve the workload. The result is below the 5% hot-cost threshold and the 1.05 replacement gate.

Linux `perf` attribution was unavailable because `perf_event_paranoid=4`. The stage-disabled probe is the declared fallback. It does not supply PRE/POST gate evidence.

## W3 classification

Classification: `fixed` (measured cold).

Review evidence preimage:

```text
PASS — W3 classification: fixed (measured cold). Probe preimage: execute_write keeps resolved_checked, overwrite guard, expand_template, create_parent, and fs::write unchanged; only journal.replace_before_mutation(...) is replaced by existing-target fs::remove_file with the same OpError mapping. Thus the sentinel is still removed and rewritten, but no StepJournal backup/inverse/root is created and terminal root cleanup has nothing to remove. Ten runs: wall median 2.35940143808s, stdev 0.10879095167; loop median 2.157474526s, stdev 0.09860587149; loop share 91.44%; POST/probe 0.9895 wall and 0.9894 loop, below the 1.05 gate. J0 adds no branch, cache, dependency, allocation, or configuration: Option::take/field resets move existing ownership and remove the postinstall to_path_buf clone plus redundant clears while preserving existing conditions. Caveats: no speedup inference; at-floor unavailable; perf attribution blocked by perf_event_paranoid=4.
```

Review evidence SHA-256: `26ac05c07997f7172aaabbb5bb31136439b8cdb6937f5aa9df88dc1fadf88e15`

`fixed` has no direct positive unit samples, measured floor, scaling claim, or keep/revert optimization action. The ledger must preserve the signed contrast. It must reject an absolute or clamped delta.

## Binding boundary

The workload and fixed classification records bind measurement evidence only. They do not complete the five Linux journal cells. Completion still requires:

- the expected Linux fixture and process evidence for each cell;
- one complete deviation proof per safety-deviation candidate;
- authenticated repository-maintainer safety-deviation approval for each exact proof digest;
- authenticated scope approval;
- an authenticated tranche review for the exact checkpoint closure;
- a reviewed landed tranche on `main`.

The macOS mirror cells remain unresolved until native macOS process integration runs.

## Ledger record schema r2

The scope schema remains version 2. The scope record did not change, and the ledger had no workload or performance records to migrate.

Workload records now bind:

- the fixture, workload source, file-system stage, toolchain, executable, and raw sample artifact digests;
- ten aligned wall-time and install-loop samples;
- a three-warmup minimum;
- median install-loop share of at least 90% of wall time.

Performance records now use one required classification: `fixed`, `at-floor`, or `hot`.

- `fixed` forbids direct unit samples, a floor, an optimization experiment, and scaling. It binds baseline and probe workloads, the exact probe source, the signed median contrast, the median ratio, and W3 review evidence. Disabling the stage must save less than 5%.
- `at-floor` requires measured hot samples and a positive floor at no more than twice the measured unit cost. It forbids an optimization experiment.
- `hot` preserves the existing measured floor and `keep` or `revert` experiment gates.

The verifier rejects performance-floor and hot-reclassification approvals for a `fixed` unit. Full workload and classification records already enter the tranche-review closure; no closure compatibility path was added.

The verifier self-test runs 88 checks. The new checks reject direct samples, floors, experiments, scaling, stale signed deltas and ratios, a 5% or larger disabled-stage saving, identical baseline/probe builds, misaligned workload receipts, invalid review evidence, false grade claims, and performance approvals attached to a fixed unit. The self-test fixture clears live evidence and event state after copying the ledger so each mutation starts from the canonical unresolved baseline. The live ledger is verified separately.
