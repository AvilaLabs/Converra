# Engineering workspace verification

This extension adds a shared study workspace, structured GUI revisions and a
local MCP server. The software checks below exercise declared inputs and retained
evidence. External engineer validation remains open under PC-08 in the roadmap.

## What was checked

The shared study tests cover named alternatives, exact case and dataset source
bytes, historical results after revision, dependency validation, corrupted
imports, live cache invalidation, review packages and explicit follow-up inputs.
Diagnosis tests distinguish lower-bound evidence from actual domain exclusions
and retain the declared quantities and limits for optional screens.

MCP integration tests launch the server as a child process and communicate through
the official SDK client over stdio. They exercise tool discovery, schemas, errors,
search completion, cancellation, sensitivity jobs, persistence, multiple material
dependencies, resources, review exports and numerical parity with the same
headless calculation. Exported manifest resources remain readable after restart.
Restarting the server leaves its execution cache empty.

Browser verification uses an isolated Chromium instance and a release Trunk build,
with screenshots and actual file uploads/downloads through Chrome DevTools
Protocol. No remote desktop connection is used. The GUI checks cover invalid and
valid structured revisions, small numerical units, responsive calculations,
cancellation, inconclusive evidence, exact repeat reuse, named alternatives,
follow-ups, comparisons, saved workspace reopening and portable review export.
Accepting a follow-up starts its calculation automatically and clears the source
variant's diagnosis while the new result is pending. Saved workspaces from before
and after an exact browser repeat were byte-identical, including the result ID
and original record bytes. The downloaded browser review package passed the CLI's
artifact, input-binding and ledger verification.

The mixed valid/invalid pack fixture remains inspectable and saveable: both
candidates appear in the map and table, and the rejected candidate's unavailable
current remains JSON `null` in the downloaded workspace.

The small-window check uses 900 × 650 pixels. Keyboard checks exercise visible
focus; they do not constitute a complete accessibility audit. Native dialogs
were not exercised through a graphical desktop in this session.

Generated logs, screenshots, source fixtures and downloaded artifacts are retained
locally under `runs/engineering-workspace-20260929235555/`, which is ignored by Git.
The reduced GUI, invalid-pack and multi-binding cases are attributed software fixtures,
not customer cases or external validation evidence.

## Measured repeat work

The `workspace_profile` example measures one cold study operation and five live
exact-input repeats, using one engine execution thread at unchanged declared
fidelity. It asserts identical result bytes on reuse, input invalidation and an
empty execution cache after reopening the saved study.

| Native fixture | Cold operation | Median of five live repeats |
| --- | ---: | ---: |
| Shipped measured-conductor first study | 52.867 s | 3.291 ms |
| Four-turn, two-tape study with two measured material bindings | 78.270 s | 8.734 ms |

Measurements were taken on Linux x86_64 with an Intel Core i3-N305, using the
repository's release profile and Rust 1.95.0. Other work was running on the host;
these are local timings, not controlled comparative solver benchmarks. Repeat
timings include the shared study operation and its evidence handling; they do
not measure GUI rendering or MCP transport latency.

Both records have search status `FAIL` and cost/acceptance agreement status `PASS`.
The first study's candidates remain `INCONCLUSIVE`; no eligible passing candidate
was selected. Agreement verifies the implemented recomputation path and does not
establish independent physics accuracy or engineering acceptance.

The numerical implementation fingerprint in both receipts matches the reviewed
source: `7e02da56a86b3d082e12779e050f96c80f1945fb3126bdd46e5235a02695039a`.
These timings support fast, faithful reuse within a live process. They do not
support a claim that Converra is the fastest or most accurate tool in its field.

## Delivery checks

All four local delivery gates passed with the repository's pinned Rust 1.95.0
toolchain, including 506 workspace tests. The final receipts are retained under
`runs/engineering-workspace-20260929235555/complete-workspace-checks/`.

| Check | Command |
| --- | --- |
| Formatting | `cargo fmt --all --check` |
| Workspace lint, warnings denied | `cargo clippy --workspace --all-targets -- -D warnings` |
| Complete workspace suite | `cargo test --workspace` |
| Browser target | `cargo check --target wasm32-unknown-unknown -p optcoil-app` |

Release browser builds and the actual workflow checks supplement those gates.
Page navigation retains each page's own scroll position, so switching from a
scrolled result view to a new study view keeps its workspace controls visible.
Each delivered commit must also pass [GitHub CI](https://github.com/AvilaLabs/Converra/actions/workflows/ci.yml).
The [source history](https://github.com/AvilaLabs/Converra/commits/main/) identifies
the corresponding delivery commits. No release tag or hosted-browser deployment
is part of this extension.

For setup and use, see the [workspace guide](ENGINEERING_WORKSPACE.md) and
[MCP guide](../crates/optcoil-mcp/README.md).
