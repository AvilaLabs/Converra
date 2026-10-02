# Scenario studies

Scenario studies compare named winding alternatives under explicit changes to
conductor prices, critical current and operating temperature. Each scenario runs
the full declared candidate search at its original numerical fidelity. Results
describe those scenarios and that bounded search space; they assign no
probabilities and do not establish engineering qualification.

These extensions ship from v0.3.0; the v0.2.0 archives predate them.

## Existing GUI

Open **Engineering study**, create the named alternatives, and use **What-if
robustness study**. Select the alternatives and edit the named what-if rows.
The nominal reference always uses unchanged assumptions. Price and Ic controls
are multipliers for each bound dataset; temperature is an offset in kelvin.
Default what-if values are editable examples, not measured uncertainty bands.

Preview the workload and resolve input errors before running. A scenario study
uses a native background job or the browser's calculation worker. Cancellation
retains previous completed evidence and attaches no partial analysis.

The history shows scenario winners, changes of preferred alternative, and
whether each alternative's nominal geometry passes primary screening. A geometry
can pass those screens while another becomes cheaper; finer acceptance checks
apply to the selected scenario winner. Unresolved comparisons and
scenarios with no eligible candidate are reported explicitly. The full source
records remain available in exported JSON.

Save the workspace to retain the analysis. Workspace v2 imports older v1 studies
and keeps up to three completed analyses within the overall 64 MiB limit. Input
revisions retain evidence as history; the current/history label checks exact
source bindings and engine identity. Reopening evidence never seeds the live
calculation cache.

## Scenario specification

Use the exact dataset IDs bound by the selected cases:

```json
{
  "schema": "optcoil-robustness-spec/v1",
  "scenarios": [
    {
      "id": "nominal",
      "name": "Declared conditions",
      "price_multipliers": {},
      "ic_multipliers": {},
      "temperature_offset_k": 0.0
    },
    {
      "id": "supplier-and-conductor-what-if",
      "name": "Illustrative price +10%, Ic -10%",
      "price_multipliers": {"robinson-superpower-ap-v3-lowfield": 1.1},
      "ic_multipliers": {"robinson-superpower-ap-v3-lowfield": 0.9},
      "temperature_offset_k": 0.0
    }
  ]
}
```

An explicit `nominal` row is mandatory, with unit multipliers and zero
temperature offset. Unknown dataset IDs and incompatible comparisons are
rejected. Alternatives must share requirements, operating conditions, limits,
numerical settings and sampling contracts. Changing a requirement creates a
different decision and cannot silently enter the same cheapest-design ranking.

Price factors affect the base conductor, graded specifications and purchased
piece offers bound to that dataset. Assembly, splice, joint and cooling terms
keep their declared values. Ic factors affect every matching material binding.
Changed data receives a distinct synthetic sensitivity identity and retains
source attribution without borrowing the original measurement label or
attestation. Temperature offsets use the supplied material domain; unsupported
queries stay unresolved.

Limits are eight alternatives, sixteen scenarios and sixty-four total reruns,
with additional aggregate candidate, work and payload caps. MCP imposes its
reported server budget and a fifteen-minute cooperative wall-time limit.

## CLI and local MCP

From a saved workspace and scenario JSON file:

```bash
optcoil study-robustness workspace.json scenarios.json \
  --variant variant-0001 --variant variant-0002 \
  --preview --output preview.json
optcoil study-robustness workspace.json scenarios.json \
  --variant variant-0001 --variant variant-0002 \
  --output scenario-evidence.json
```

Outputs must be new files. The CLI artifact contains the exact original and
transformed inputs, source and derived material bundles, execution options,
completed calculation records, scenario summaries and fingerprints for offline
review and replay. It does not overwrite the source workspace.

The local MCP server provides `preview_robustness`, `start_robustness` and
`list_robustness_results`. Preview/start accept `variant_ids` and the exact
`spec_json` string. Poll `get_job`; read the returned
`optcoil://study/robustness/<record-sha256>` resource for full evidence. Completed
analyses persist with the workspace and remain readable after a server restart.
The server's normal mutation, cancellation and bounded-job rules apply.

## Reading the decision

A selected winner requires the declared search and independent cost/screening
agreement gates. An unresolved alternative prevents a supported global ranking.
A conclusively rejected alternative does not become a winner. Cost ranges and
regret describe evaluated comparable choices, without extrapolating to absent
designs or scenarios.

Offline import checks bind sources, transformations, calculation ledgers and
derived summaries. They check artifact integrity and agreement with the declared
model; independent physical validation still needs suitable reference evidence.

See [engineering workspaces](ENGINEERING_WORKSPACE.md) and
[representative workflow measurements](WORKFLOW_COMPARISON.md).
