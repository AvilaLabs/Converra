# Decision clarity extension: software verification

This records software checks for the September 2026 diagnosis, scenario-study
and workflow-comparison extension. It does not close the external engineer
validation gate PC-08. These changes postdate the published v0.2.0 archives.

## Local gates

The final Rust source snapshot passed:

- `cargo fmt --all --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- `cargo test --workspace`: 520 tests passed, zero failures
- `cargo check --target wasm32-unknown-unknown -p optcoil-app`
- Release CLI/MCP and workflow-comparison example builds
- Optimized Trunk browser build

The ignored evidence directory is `runs/decision-clarity-20260930143200/`.
`gate-receipts.json` records command exits and durations;
`gate-source-hashes.json` fingerprints the Rust files, and
`gates-complete.json` confirms they did not change during verification.
GitHub CI must pass for the delivered commit before completion is reported.

The new regression checks cover diagnosis priority and scope, graded and
purchased-piece repricing, synthetic Ic identities, incomparable inputs,
bounded repeated source payloads, cancellation, tri-state geometry survival,
tampered calculation/winner/summary evidence and portable reopening. MCP stdio
tests exercise discovery, preview, asynchronous jobs, cancellation, direct-engine
parity, retained history and restart. Repeated identical inputs keep distinct
full-record resources when runtime metadata differs; reading the first resource
still returns its exact earlier artifact.

## Existing egui interface

Verification used the compiled application in an isolated local Chromium browser
through CDP. The retained screenshots include:

- `before-study.png`: the original first-screen diagnosis layout.
- `final-prioritized-diagnosis.png`: a completed measured-data search with three
  prioritized action groups and expandable decision evidence, optional checks
  and the complete raw ledger.
- `gui-ap-price.png` and `gui-preview-details.png`: two named alternatives,
  dataset-specific controls and a four-run aggregate preview.
- `gui-history-outcomes.png`: six retained native scenario records, including
  explicit rejected alternatives.
- `gui-historical-label.png`: retained analysis marked historical after a
  declared price revision.
- `gui-cleared-alternatives.png`: clearing the last selection stays cleared.
- `gui-invalid-details.png`: invalid temperature blocks calculation and keeps
  earlier evidence.
- `gui-worker-running.png`: a live browser scenario worker.
- `gui-cancellation-proof.png`: cancellation acknowledged in the interface.
- `gui-small-window-history.png`: a 900 × 650 window with accessible scenario
  controls and scrollable history.

The native scenario exercise used the attributed two-binding software fixture
and a separately identified all-Shanghai alternative. Three explicit scenarios
covered nominal inputs, an illustrative AP price increase, and a combined price,
Ic and temperature change. All six calculations rejected their bounded choices;
there was no supported winner or winner switch. The GUI's exported JSON matched
the native artifact semantically, and its saved workspace reopened with all
six source records intact.

A separate browser exercise restricted OC-019 to its declared baseline geometry,
retaining its original requirement, material, numerical and sampling contracts.
Four scenario rows completed as unresolved, without a supported global ranking.
The saved workspace retained that analysis beside the earlier native evidence.
A subsequent job cancelled before completion. The entire saved workspace matched
its pre-cancellation contents: two earlier completed analyses and no partial
attachment. `gui-cancellation-receipt.json` records that comparison.

These exercises use attributed software fixtures and illustrative prices. They
are not supplier quotes or customer design validation. Winner-switch and
missing-winner aggregation also have explicit regression controls; the observed
GUI exercises above do not claim a successful engineering design.

## Workflow measurement

The matched-input measurement protocol and observed results are recorded in
[WORKFLOW_COMPARISON.md](WORKFLOW_COMPARISON.md). CLI/files and the shared
workspace use the same engine and numerical settings. The comparison measures
software operations and reuse; human effort, physical accuracy and superiority
over commercial tools require separate evidence.
