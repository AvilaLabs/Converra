# Read your results

Read the decision, individual checks and limitations before the cost total. A search can finish successfully as a computation while producing no eligible design.

| Verdict | Meaning |
| --- | --- |
| `PASS` | The evaluated check met its declared requirement and model gates. |
| `FAIL` | An evaluated requirement or agreement gate failed. |
| `INCONCLUSIVE` | Data coverage, model applicability or fidelity prevented a determined answer. |
| `NOT_EVALUATED` | The check was not performed. |

## Three levels to inspect

**Candidate screening** evaluates a geometry at the stated sampling plan. Inspect peak conductor field, critical-current utilization, geometric clearance and every configured screen. A sampled pass says nothing about locations the plan did not resolve.

**Search and selection** describe the campaign as a whole. A bounded or cancelled search may retain an incumbent without establishing an optimum. An unresolved comparison or a baseline that cannot pass its refined check can prevent a recommendation even when a candidate passes a coarse screen.

**Acceptance and verification** answer different questions. The acceptance path separately recomputes costs and screening checks, using finer sampling where declared. Its physics assumptions remain shared with the search. Offline `verify` checks input bindings, record structure and ledger arithmetic; physical validation needs suitable independent evidence.

## Read a savings figure

Compare the baseline and candidate under the same requirements, material policy, fidelity and prices. Savings are the reduction in modeled total cost relative to that baseline. Synthetic prices support an illustrative comparison; a quoted price needs its source and commercial assumptions recorded.

Read the installed and purchased conductor lengths, manufacturing terms and missing checks alongside the percentage. Structural analysis, thermal design, quench protection and manufacturing qualification require their own evidence.

See [benchmarks](benchmarks.md) for worked interpretations and [troubleshooting](troubleshooting.md) when a study cannot select a design.
