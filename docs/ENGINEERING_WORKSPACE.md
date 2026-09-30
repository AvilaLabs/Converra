# Engineering study workspace

An engineering study keeps a baseline and proposed alternatives together with
their exact source data and calculated evidence. The GUI and local MCP server
use `optcoil-search::study`. Source builds save `optcoil-study-workspace/v2`,
including retained scenario evidence, and import older v1 workspaces. The
published v0.2.0 archives use v1 and predate the scenario extension.

The extension's local gates and observed GUI workflows are recorded in
[decision clarity verification](DECISION_CLARITY_EVIDENCE.md).

## GUI workflow

1. Open a supported coupled-search case, or use the guided **New case** builder.
   Load any external conductor dependencies on **Materials**.
2. Review **Applicability and work estimate**, then run the search. Search
   completion, screening, numerical checks and engineering acceptance retain
   their separate meanings.
3. Use **Revise** for routine changes with units: field target, temperature,
   conductor, pack choices, prices and declared limits. Unsupported geometry
   controls are disabled. The advanced JSON editor covers other declarations;
   invalid input must be corrected before saving.
4. Open **Engineering study**. Name variants, duplicate an alternative and compare
   its inputs and calculated decision against the original.
5. Inspect **Constraint diagnosis**. Source builds rank and group next actions,
   with optional unperformed checks and the raw ledger available in collapsed
   details. A reported quantity and limit belong to the
   stated check. A location is shown only when the record supplies it; absent
   material data and unresolved model applicability remain visible.
6. Review a **Follow-up experiment** proposal before creating and calculating it.
   Tape-count and strand-count proposals extend one pack-choice axis; they keep
   the field requirement, declared limits and numerical gates fixed. A proposal
   has no calculated result until its search completes.
7. **Save workspace** to keep variants and evidence together. **Open workspace**
   validates its stored sources and records. Export a **Review package** for a
   standalone report, inputs, conductor dependencies and offline verification.

The workbench also autosaves a recoverable draft on the current device. If a
saved draft is found at startup, choose **Restore draft** or **Discard draft**;
restoration brings back the active source, workspace, unfinished case edits,
resolved material bundles and completed evidence. An interrupted calculation
is not represented as completed and can be run again. Desktop drafts use the
application configuration directory; browser drafts use this browser's local
IndexedDB storage. Browser storage does not move between browsers or devices.
Unfinished import mappings and their original file are retained too; validate
the restored source again before applying it. Concurrent tabs/windows cannot
silently replace a newer draft. If storage reports a conflict, retry to inspect
the latest saved draft, then choose restore or discard while keeping the current
session. Desktop close waits for a saved draft, or lets you export the study or
explicitly close without saving. Historical recovered artifacts are available
from **File → Export recovered historical evidence**.

Export a study workspace or review package for a portable copy; device-local
recovery is not a substitute for export.

For a screening PASS comparison, **File → Examples → Measured supported
comparison (2 candidates)** provides an optional OC-007-derived case with the
original numerical and engineering gates. It takes several minutes; the
ordinary first-study example is quicker and demonstrates unresolved data
coverage explicitly.

For a small two-candidate comparison, open the first-study case, run it, then
duplicate it as a named study variant. Change one supported choice or declared
input, review applicability again, and calculate the variant. Compare the
baseline and alternative in **Engineering study**; each result remains tied to
its own exact case and material sources. If the alternative is not run, the
comparison correctly has no calculated result for it.

For explicit supplier-price, conductor-Ic and temperature what-ifs across named
alternatives, use [scenario studies](ROBUSTNESS.md). Completed analyses retain
source inputs and full evidence; input changes mark them historical.

Desktop searches run on a background thread. Browser searches use a Web Worker
and one execution thread; imported desktop options require an explicit browser
override before running. A successful browser worker stays available for repeat
calculations; cancellation, errors or reopening terminate it and clear its
execution cache. Cancellation produces no completed partial result. Previous
completed evidence remains available.

## Current and historical evidence

Each result retains the case bytes, dataset bundle bytes and execution options
used to produce it. Revising an input preserves the old result and its
dependencies; it becomes historical evidence. Decision comparisons use results
bound to the current exact inputs, so an unevaluated alternative cannot inherit
the original variant's result.

Offline verification checks artifact bindings and cost arithmetic. It does not
independently validate physics. Imported result files are evidence supplied by
their producer, even when their bindings verify.

Undefined current or field quantities on a candidate that failed before
screening are serialized as JSON `null`. They are unavailable quantities, not
zero. The artifact verifier rejects missing quantities on evaluated candidates.

## Repeat calculations

`StudyEngineSession` can reuse a completed, verified result calculated by the
same running process. Its key includes exact case bytes, every supplied raw
bundle, execution options and engine identities. Reuse preserves the original
record bytes and calculation time; it does not claim a new physical evaluation.

Any relevant input change produces a cache miss. Reopening a workspace or
importing a record never seeds this execution cache. Those records remain
viewable, comparable and exportable.

For a reproducible local timing receipt at unchanged fidelity:

```bash
cargo run --release -p optcoil-search --example workspace_profile -- \
  benchmarks/coupled/first-study.json runs/my-iteration-measurement
```

Use a new output directory. Optional bundle paths follow the directory argument.
The receipt measures one cold study operation and five exact-input live reuses,
checks record equality and input invalidation, and confirms a reopened workspace
starts with an empty execution cache. This measures that bounded operation; it
does not establish superiority over other solvers or user validation.

## AI agent workflow

See the [MCP guide](../crates/optcoil-mcp/README.md) for local setup, typed tool
calls, result resources and operating limits. Save or read the workspace resource
to move the same study between MCP and the GUI. Use one active writer for a study
directory; the server is a local process with its own session cache.

See the [workspace verification evidence](ENGINEERING_WORKSPACE_EVIDENCE.md) for
exercised workflows and measured repeat-work scope, and the
[self-directed workflow evidence](SELF_DIRECTED_WORKFLOW_EVIDENCE.md) for
spreadsheet intake, cancellation, portable reopening and unfinished draft recovery.
