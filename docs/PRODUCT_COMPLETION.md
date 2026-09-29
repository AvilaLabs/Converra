# Product completion evidence

The September 2026 roadmap extension targets a complete supported study from
existing inputs to a reviewable decision. Its software checks are distinct from
the external engineer validation gate.

## Implemented workflow

| Roadmap item | Implementation and evidence |
| --- | --- |
| PC-01 | Record-aware cost metrics; restricted scalar scenarios; reprice v2; graded ledger/BOM/report regression. |
| PC-02 | Full JSON revision and duplication with validation and source protection; input differences; case round-trip and workbench state regressions. |
| PC-03 | Small attributed measured-conductor startup case, explicit synthetic example, applicability sequence and geometry preview. |
| PC-04 | Shared headless preflight in the CLI and workbench, dependency checks and sampling/work proxies. |
| PC-05 | Bounded metadata/CSV intake and automatic hash binding; documented price categories and assumptions; packaged material bundles. |
| PC-06 | Shared decision summary and portable review package; artifact, identity and modeled cost verification, including tamper rejection. |
| PC-07 | Previous results retained during replacement; cancellation in search and acceptance; independently recomputed baseline reuse. |
| PC-08 | Headless first-study workflow through revision, cancellation, comparison, export and verification; native and wasm gates. External validation remains open. |

The first study deliberately produces an inconclusive current-capacity screen
because its self-field ratios exceed the declared limit. It demonstrates a
diagnostic decision with explicit next actions and no procurement recommendation.
It does not manufacture a passing design. The larger OC-007 reference provides a
passing-option example within its declared scope. Both use reference inputs,
not customer evidence.

See [the tutorial](TUTORIAL.md), [dataset intake](DATASETS.md), and
[release measurements](PRODUCT_PERFORMANCE.md). The measured acceptance-reuse
path removed a duplicate rerun, with matching costs and verdicts. Its single
before/after measurement does not establish a general speed ranking.

## Verification gates

On 2026-09-29, formatting, workspace Clippy, all 484 native tests and wasm
compilation passed. The release CLI generated the first-study diagnostic record
with unchanged costs and verdicts and checker v19. Diagnostic and graded review
packages passed verification as directories and after independent extraction
with Python's standard tar reader. The graded scalar-price request was refused.

Run from the repository root before each push:

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo check --target wasm32-unknown-unknown -p optcoil-app
```

The workflow regression runs real calculations against attributed conductor
data. Package checks reject altered inputs and inconsistent ledgers, including
cases where hashes are rewritten to match a tampered record. Release CLI checks
also exercise directory and tar exports and verification after extraction.
The delivery handoff records the pushed commit and its green GitHub CI run.

Verification establishes software behavior and artifact consistency. A shared
physics implementation is not an independent physical validation. A browser
compile check is not an observed interactive browser session; the revised UI
also needs use and observation in the external session.

The frontends currently accept one external dataset bundle per search, alongside
embedded dependencies. Packages retain all referenced resolved bundles, but a
case with multiple distinct external dependencies needs additional frontend
support before it can be rerun there. Unsupported material queries retain their
unresolved status. Structural, thermal, quench, manufacturing and full-width
conductor qualification remain governed by the declared model scope.

## Open external validation gate

The goal remains open until an identified engineer completes an actual supported
decision using authorized inputs. No participant or customer input has been
supplied for this gate.

To conduct the session:

1. Identify the participant, decision, existing process and success criterion.
2. Store authorized case, conductor and price inputs under ignored
   `customer-data/`; preserve attribution, units, domains and quote assumptions.
3. Observe the participant importing, reviewing applicability, calculating,
   interpreting unresolved checks, revising and comparing an alternative.
4. Record elapsed effort, assistance, failures and unresolved engineering work.
   Compare the decision and costs with their existing method at matched inputs
   and fidelity; investigate disagreements.
5. Have a second reviewer verify and interpret the exported package without the
   author's directory. Record what they can and cannot conclude.
6. Resolve observed software blockers and retain an authorized, appropriately
   redacted session record before closing the roadmap gate.

Reference fixtures, invented quotes and assumed user observations cannot close
this gate. Until it is complete, the software milestones support a reviewable
product candidate; they do not justify "best in class," "fastest," or "most
accurate" claims.
