# Representative workflow comparison

This harness compares the nearest reproducible alternative available in this
checkout: an explicit headless CLI workflow over case, dataset, run-record, and
review-package files against a named shared-study workflow using
`StudyWorkspace` and `StudyEngineSession`. Both paths use the same frozen case
bytes, raw dataset bundle bytes, one-thread execution option, base run, named
5% price revision, revised run, comparison, exact repeat, save/reopen, and
portable review-package verification.

The explicit-file path is a practical reproducible baseline for this local
comparison. It is not a paid solver or a claim against Allsolve, COMSOL,
Ansys, or another commercial engineering platform. The harness counts actual
CLI process invocations, API operations, and files. Counts and software
timings do not estimate human setup or review effort. Only external user
research can establish how long either workflow takes an engineer.

The receipt keeps these count units separate: `explicit_cli_command_count`
counts commands in the explicit-file path, `all_cli_subprocess_invocation_count`
also includes CLI verification of the shared workspace's package and records,
and `workspace_operation_count` counts named Rust API calls made by the
example. These counts are not user step counts and are not directly comparable.
Artifact counts are files found on disk.

## Fixtures and run command

`benchmarks/workflow-comparison/fixtures.json` pins raw SHA-256 hashes for the
case and every bundle before the run. It contains three attributed software
fixtures: the existing measured-conductor first study; a four-turn/two-tape
case with separate measured SuperPower AP and Shanghai HFLT bindings; and a
controlled two-binding variant with a declared 20 T bore-field requirement to
exercise the failed-search path. The fixture provenance identifies illustrative
prices and excludes customer validation. The revised case files are derived
deterministically from each frozen case by increasing one declared price by 5%;
they are snapshotted and hashed alongside the inputs.

The measurement source and licensing notes remain in the checked-in
[low-field SuperPower AP dataset](../data/materials/robinson-superpower-ap-v3-lowfield/README.md),
[SuperPower AP dataset](../data/materials/robinson-superpower-ap-v3/README.md), and
[Shanghai HFLT dataset](../data/materials/robinson-shanghai-hflt-v3/README.md).

Build the release CLI and the workflow example once, then run them sequentially
from the repository root. Use a new output directory under ignored `runs/`:

```bash
cargo build --release -p optcoil-cli
cargo build --release -p optcoil-search --example workflow_comparison
python3 tools/workflow_comparison.py \
  --optcoil target/release/optcoil \
  --workspace-comparison target/release/examples/workflow_comparison \
  --run-dir runs/workflow-comparison-20260930
```

The executable arguments let a controlled run reuse already-built release
binaries. Each fixture receipt records CLI arguments and exit codes, typed
workspace operations, artifact paths/counts, input hashes, search records,
semantic equivalence with a fixed allowlist of elapsed-time/timestamp fields
excluded (all hashes, provenance, verdicts, and numerical outputs remain
compared), and distinct
fresh-search, orchestration, and exact-repeat timings. A nonzero search exit is
retained when the CLI has still emitted its valid `FAIL` or `INCONCLUSIVE`
record; verification and package errors remain fatal. The CLI exact repeat is a
new process and a fresh solve. The workspace exact repeat is in the same live
engine session and should reuse a verified result; after reopening, a new
session must have an empty cache. Process peak memory is not captured by this
harness because portable attribution to the child process is not available in
the current runner.

## Observed results

One complete sequential sample was recorded on 2026-09-30 at 17:03 UTC. This is
a single development-host observation, not a controlled performance study.
Other host activity, including parallel GUI validation, was not controlled or
measured. The explicit CLI and workspace paths were run one after the other for
each fixture, with one execution thread in both paths. Do not generalize these
times to other machines or users.

Host: Linux 7.0.0-34-generic, x86_64, Intel Core i3-N305, 8 logical CPUs;
one solver thread. Toolchain: rustc 1.95.0 (59807616e, 2026-04-14), Python
3.14.4. Source revision: `25f11fe4842571686adbe4a3fe1de8ec464f4915` with a
non-clean worktree. The suite receipt freezes the Rust-source, Cargo input,
lockfile, binary, harness, fixture-manifest, and per-fixture input hashes; it
also records the worktree status. The exact release binary hashes are CLI
`8eeaa05bdcda93f02847370750b00f2cb567948d7422028e4420c19bcd5651cc` and
workspace example
`1f020ece269d08e84945eabd23aef5bf8458c70565bd6494e10c5e2306e0e588`.

Fresh-solve columns show base/revised seconds. CLI figures are fresh-process
wall time for each search command; workspace figures are each record's solve
time inside one fresh `StudyEngineSession`. Exact-repeat time is shown
separately: the CLI starts another fresh process and recomputes; the workspace
repeats in the same session and reuses the completed result. Workspace
orchestration excluding its run calls was 0.096, 0.151, and 0.089 seconds,
respectively; shared-workspace process wall time was 92.824, 246.692, and
215.991 seconds. These software timings do not measure human setup or review
time.

| Fixture | CLI base / revised search and candidate statuses | Workspace base / revised search and candidate statuses | Semantic record parity / repeat parity | Fresh CLI search (base / revised, s) | Workspace fresh solve (base / revised, s) | CLI exact repeat (s) | Workspace repeat (s) |
| --- | --- | --- | --- | ---: | ---: | ---: | ---: |
| Measured startup | FAIL / FAIL; both candidate sets INCONCLUSIVE | FAIL / FAIL; both candidate sets INCONCLUSIVE | Base + revised equal; both repeat checks equal | 52.064 / 46.024 | 45.818 / 46.797 | 46.034 | 0.055 (reused) |
| Two measured bindings | FAIL / FAIL; one candidate FAIL in both | FAIL / FAIL; one candidate FAIL in both | Base + revised equal; both repeat checks equal | 160.212 / 257.296 | 144.907 / 101.538 | 118.069 | 0.028 (reused) |
| Controlled failure | FAIL / FAIL; one candidate INCONCLUSIVE in both | FAIL / FAIL; one candidate INCONCLUSIVE in both | Base + revised equal; both repeat checks equal | 112.056 / 108.042 | 110.492 / 105.331 | 110.059 | 0.022 (reused) |

For all six base/revised pairs, semantic comparison retained input and case
hashes, datasets and their hashes, verdicts, candidate outcomes, and numerical
results; the compared records were equal after removing only explicitly
allowlisted runtime and timestamp fields. The CLI's exact-repeat records were
semantically equal to the original; the workspace repeat returned the same
result ID, and reopening into a new session started with an empty cache. All
three workflows therefore agreed on the observed results, but none produced a
selected geometry or cost. Each `acceptance_agreement_status=PASS` is the
record's internal agreement check; the search status was `FAIL`, so these are
not successful engineering decisions.

Across each fixture, the harness recorded 9 explicit-file CLI commands, 14
named workspace operations, and 56–59 output files. The explicit-file workflow
also performed two local file/index operations; its 9 command count excludes
CLI verification commands the harness uses for the workspace path. Including
those workspace verifications, each fixture receipt records 12 CLI process
invocations. These are software operation and artifact counts, not human
steps. Process peak memory was not captured.

The machine-readable source of these observations is
`runs/workflow-comparison-20260930-decision-clarity/suite-receipt.json` and its
three fixture `receipt.json` files. The run directory is ignored and is not
part of the source tree; retain it alongside this document when archiving the
measurement.

Report setup/revision/review **effort** only after a separately designed,
attributed user study records actual participant actions and elapsed time.
Do not convert the command or API counts above into human minutes. These local
measurements cannot establish accuracy or superiority over paid alternatives.

## Bluemira matched-model comparison

The checked system Python, the existing `thermal` and `w003env` virtual
environments, and the ACTINV `ci-venv` and `smoke-venv` resolve no `bluemira`
module. No independent Bluemira runtime was found in the inspected local
environments, so this task does not report a new numerical or timing result.
The prior independent-kernel protocol and historical values are documented in
[OC-011](OC011.md); those values are not results of this workflow harness.

A feasible follow-up is the OC-011 Phase B finite-cross-section comparison:
run the exact same frozen case geometry, ampere-turns, and probe deck through
Converra's finite-cross-section racetrack evaluator and Bluemira's
`ArbitraryPlanarPolyhedralXSCircuit`. Use the same declared rectangle and
uniform current density, then compare the complete `(Bx, By, Bz)` vector at
every frozen probe under OC-011's predeclared magnitude and direction gates.
Record the deck and source hashes, solver versions, numerical tolerances, and
per-probe residuals. For runtime, measure fresh-process import-plus-evaluation
separately from in-process repeated evaluation, keep the probe deck and
discretization identical, and record process peak RSS on the same host. This
would compare two implementations of one declared field model; it would not
compare optimization, conductor selection, or engineering acceptance. Run it
only once an existing Bluemira environment is available; this task does not
install CAD/FEM dependencies.

## Limits

Search status and acceptance agreement are reported exactly as calculated.
They do not certify the physical adequacy of screening assumptions. A `FAIL`
or `INCONCLUSIVE` result is retained as software evidence and is not presented
as a successful design decision. The comparison describes the installed local
CLI and workspace APIs only; it does not measure a desktop user's GUI latency,
MCP transport, customer setup burden, commercial solver workflows, or
independent physical accuracy.
