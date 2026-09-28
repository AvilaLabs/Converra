# Converra — technical summary

What the product does, what it proves, and what it does not claim.
Written for a technical evaluator. September 2026.

Converra (internal engine: OptCoil) is a magnet-conductor design
optimizer for REBCO/HTS coils. Given a fixed magnet specification —
field requirement, aperture, geometry family, conductor library, cost
model — it searches conductor-pack and geometry choices, screens every
candidate against declared engineering gates, and emits a hash-bound
evidence record for each accepted design. The output is not "here is a
cheaper coil"; it is "here is a cheaper coil, here is exactly what was
checked, and here is what remains unverified."

## The verdict vocabulary

Every candidate and every engineering check lands in one of four
states, and the states are never blurred:

| Verdict | Meaning |
|---|---|
| `PASS` | The check ran and the design satisfies it |
| `FAIL` | The check ran and the design violates it |
| `INCONCLUSIVE` | The available data or model cannot resolve the check — the design is neither passed nor failed |
| `NOT_EVALUATED` | The check is declared but no evaluation exists for it |

Search completion is not engineering acceptance. A search that
finishes every evaluation still reports `INCONCLUSIVE` candidates as
inconclusive. The product's commercial value depends on this
distinction being real, so it is enforced in the record schema rather
than in documentation.

## What is independently checked

Acceptance is not the search grading its own homework. A separate
acceptance path re-derives candidate cost and constraint status from
the declared case, using an independent implementation for the
load-bearing physics:

- **Independent reference implementation.** The coupled-field
  benchmark (OC-004) is verified against a second implementation of the
  field and critical-current evaluation — written separately, in a
  different language, comparing all 90 evaluated points value-for-value.
- **Built-magnet parity.** Given the same requirement as a physically
  wound and tested HTS dipole (Feather-M2, CERN/EuCARD2), the verified
  optimum tracks the real program's conductor usage to ~3%
  (OC-010).
- **External cross-check.** Coil-field results agree with an
  independent magnetics code path to ~5×10⁻⁴ relative (OC-011).
- **Hash-bound records.** Each run record binds the exact input case by
  content hash; a record cannot be silently re-pointed at different
  inputs. Campaign manifests bind sequences of records the same way.

## Material data provenance

Critical-current data carries a declared data class, checked at parse
time and bound into every record that uses it:

- `measured` — laboratory-measured Ic(B, T, θ) tables. The current
  production set is the Robinson Research Institute public database
  (CC-BY), covering a SuperPower-class REBCO tape to 8 T.
- `measured_with_model_extension` — the measured rows byte-identical,
  plus explicitly labeled modeled rows above the measured ceiling.
  Modeled points are marked per-point in every record; a modeled
  verdict can never be reported as measured-verified.

The engine refuses to extrapolate: a query outside the supported
domain returns `INCONCLUSIVE`, not an interpolated guess. Schema
versions bind data classes to file versions, so a modeled dataset
cannot be re-labeled as measured without breaking parsing.

## Current results

On a startup-scale demonstration dipole spec (≥ 5 T over an 80 mm
aperture, racetrack family, declared conservative baseline):

| Result | Conductor reduction | Basis |
|---|---:|---|
| OC-012 verified optimum | ~20% vs declared baseline | Fully on measured data; independently re-checked |
| OC-016 sensitivity study | ~75% vs same baseline | Model-informed — requires measured data above 8 T to certify |

The gap between the two rows *is* the product's honesty: the thin-pack
designs that produce the larger number see 9–17 T conductor fields,
above the measured dataset's 8 T ceiling. Under a labeled model
extension they pass with margin; whether that margin is real requires
the measured data — which is currently being requested from the group
that published it. The tool's job is to make the distinction
unforgeable, not to wish it away.

## What is not yet modeled

Declared gaps, stated rather than implied:

- Stress, strain, and structural mechanics beyond a published
  stack-level Lorentz bound screen
- Thermal behavior, cooling, AC losses, quench
- Joint resistance and joint heating
- Full self-field critical-state resolution (a uniform-transport
  self-field correction is implemented and versioned; critical-state
  effects remain flagged)
- Field components along the transport direction are handled by an
  explicitly declared bounded model (`transverse_bound`), labeled
  per-point; the unmodeled default excludes such points as
  `INCONCLUSIVE`

A `NOT_EVALUATED` row in a record is a promise that the check exists
and was not run — not a silent omission.

## Deployment and data custody

The engine runs locally. A customer's case file — their real
specification, their conductor pricing, their geometry — never leaves
their environment. Exported run records are hash-bound so a customer
can hand Avila Labs (or an auditor, or their own management) the
evidence without handing over the inputs. Licensing and delivery
mechanics are product work in progress; the custody property is already
true.

## Where the numbers come from

Cost figures in records derive from each case's declared cost model —
conductor price per meter, processing, assembly, joint costs. Public
benchmarks use a placeholder or a published band, always labeled.
Real-dollar savings for a customer engagement use the customer's own
conductor quotes; the machinery is identical, the inputs are theirs.
