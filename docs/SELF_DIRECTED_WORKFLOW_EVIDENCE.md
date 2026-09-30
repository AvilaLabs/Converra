# Self-directed study workflow: software verification

This records the September 30, 2026 importer, decision presentation and draft
recovery milestone in the existing egui workbench. These features postdate the
v0.2.0 desktop archives.

## Observed checks

The final Rust source passed formatting, complete workspace Clippy with warnings
denied, all 550 workspace tests, the wasm app check, native desktop/CLI/MCP
release builds and the optimized Trunk web build.

The complete 69-step browser plan passed all eight phases against the delivery
bundle. Both imported-data searches produced no supported winner. The comparison
retained `FAIL` search results, `NOT_EVALUATED` selected results and absent winner
costs, with only `/cost/price_usd_per_m` changing. The original table's coverage
gaps remained visible. The actual sweep stayed responsive and cancelled without
replacing the earlier completed result.

The downloaded workspace retained both named variants, their imported dataset
bundles and both exact completed records. Reopening after reload and explicit
draft discard changed the new session from zero results to two. Restoring the
unfinished price edit and accepting it produced the exact $36/m case hash below;
the earlier records remained historical rather than becoming current.

The exact CI command, `node tools/run_gui_workflow_check.mjs`, also passed with
its build step enabled: exit code 0, all 69 steps, in approximately 149 seconds
on the development host with cached Rust dependencies. Its receipts and
screenshots are in ignored `runs/self-directed-20260930/gui-root-unskipped/`.
`build-info.json` records asset hashes matching `web-delivery/` and the earlier
successful `gui-full-final-prebuilt3/` run exactly. Every delivered commit
requires successful GitHub CI for that exact SHA.

## Verification protocol

The checked-in browser plan imports the attributed Robinson SuperPower AP
measurement CSV and source metadata through actual file pickers, confirms the
measurement declarations, and binds the validated dataset to the first-study
case. It calculates that case, duplicates a named variant, changes only the
tape price from $30/m to $35/m, calculates again and compares both decisions.

The plan then starts and cancels an actual margin sweep while retaining both
completed search records, downloads and reopens the portable workspace, edits
the price to $36/m without accepting the revision, reloads, restores the saved
draft and accepts that specific recovered edit. The last edit makes the earlier
$35/m calculation historical; recovery does not manufacture a current result.

The exact case identities used by the assertions are:

| Stage | Case SHA-256 |
| --- | --- |
| Imported baseline, $30/m | `788f41ae698faeb415728c516115a1c3c99d5b325071d96ba97cd964b2ceabde` |
| Calculated price variant, $35/m | `c55301d1b9c7806619e9afe09f6182b2dac689ff32021b735108775ed25975e6` |
| Accepted recovered draft, $36/m | `86b1c2a1bf2299c1cd1fd69edb34cb9d2caf14bb5031cd2c920933a1f34d5a5b` |

The browser receives genuine canvas input and file uploads through CDP. A
read-only `?verify=1` state bridge supplies assertions about source identities,
results, worker activity, comparison contents and recovery state. It has no
action hooks. Each step retains a screenshot, accessibility tree and status
receipt. The launcher records the tested web asset hashes and uses an isolated
profile and loopback server. The same launcher runs in GitHub CI.

## Native and shared regression checks

The native tests exercise actual workbench actions, background sweep dispatch,
cancellation, completed-result retention, case revision, portable reopening and
recovery. They also check native window close events, failed saves, competing
writers, invalid unfinished editor content, corrupt snapshots, exact case/run
binding and imported bundle retention. These are application action tests;
they do not claim an automated desktop window journey.

The headless importer tests cover CSV/TSV/XLSX parsing, explicit unit and nominal
coordinate policies, measured bridge-width normalization, malformed or oversized
workbooks, formula rejection, provenance receipts and later Ic scaling with
bundle reopening. Imported measurements remain distinct from synthetic data.

## Optional supported example

`benchmarks/self-directed/oc007-two-candidate.json` narrows the frozen OC-007
candidate choices to 120 turns with three or four tapes. Its provenance records
the source hash. Physical declarations, measured low-field dataset, sampling,
material policies, cost assumptions and numerical gates remain unchanged.

An observed release CLI run took 559,288 ms on the development host: three tapes
failed, four tapes passed the declared screening, and the independently
recomputed search agreement passed. Both candidate numerical statuses remained
`INCONCLUSIVE`; no reference evidence or engineering acceptance was added.
This optional example is not a fast-start timing guarantee. Its ignored receipt
is `runs/self-directed-20260930/receipt.json`.
