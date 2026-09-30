# Engineering study workspace

An engineering study keeps a baseline and proposed alternatives together with
their exact source data and calculated evidence. The GUI and local MCP server
use `optcoil-search::study` and the `optcoil-study-workspace/v1` format.

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
5. Inspect **Constraint diagnosis**. A reported quantity and limit belong to the
   stated check. A location is shown only when the record supplies it; absent
   material data and unresolved model applicability remain visible.
6. Review a **Follow-up experiment** proposal before creating and calculating it.
   Tape-count and strand-count proposals extend one pack-choice axis; they keep
   the field requirement, declared limits and numerical gates fixed. A proposal
   has no calculated result until its search completes.
7. **Save workspace** to keep variants and evidence together. **Open workspace**
   validates its stored sources and records. Export a **Review package** for a
   standalone report, inputs, conductor dependencies and offline verification.

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

See the [verification evidence](ENGINEERING_WORKSPACE_EVIDENCE.md) for exercised
workflows, measured repeat-work scope and the separate external validation gate.
