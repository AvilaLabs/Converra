# GPU field-evaluator scoping

October 2026. The coupled search spends essentially all wall time in
`RacetrackEvaluator::evaluate` — oc-023-seam: ~19.9 G kernel
evaluations in ~17.5 min on 4 threads (~19 M evals/s). The work is
embarrassingly parallel across probes, and candidates are already
parallel across threads with bit-identical results pinned by test.

## What the kernel actually is — and why that shapes the port

`optcoil-physics::racetrack` (model
`planar-path-uniform-volume-duffy-gauss-graded-metric/v4`) is a direct
volume Biot–Savart integration over a rectangular winding pack on a
planar path:

- **Far cells** (separation > 1.5 × cell diameter): ordinary tensor
  Gauss–Legendre quadrature — uniform per probe, fixed cost, trivially
  SIMD-friendly.
- **Near cells**: partition at the observer's clamped source
  coordinates, map each octant through three Duffy pyramids, graded
  near rules generated *per cell per probe*. This branchy adaptive path
  is the accuracy story — it is what makes on-pack and inside-pack
  point values correct — not incidental complexity to flatten.
- Summation is order-sensitive compensated accumulation; the
  per-probe work limit (`MAX_SAMPLES_PER_PROBE = 20M`) is a hard gate
  that must remain.

A naive "one thread per quadrature point" port would be wrong twice
over: it would blow past the work limit's intent and it would not
reproduce the near-field treatment that the field values depend on.

## The honest design: hybrid evaluator

- **GPU handles the far-field pass.** Per probe, every far cell
  contributes a fixed-order tensor product — a dense, regular,
  f64-or-fixed-point kernel. This is the bulk of evaluations for
  off-pack probes and a large share even for on-pack ones (most cells
  of most candidates are far from most probe points).
- **CPU keeps the near-field pass.** Cells within 1.5 × diameter of the
  probe keep the existing Duffy path — branchy, adaptive, low volume.
  The split is per (cell, probe) pair by the same separation predicate
  the CPU already computes.
- **Summation contract.** GPU partial sums must combine deterministically:
  fixed reduction order per probe, or pairwise/Kahan on-device with the
  combine order declared as part of the model id. The CPU near-field
  contribution adds into the same compensated accumulator last.
- **Model identity.** A GPU path changes floating-point summation
  order ⇒ not bit-identical to the CPU evaluator ⇒ it is a **new model
  id**, and the record must name it. `search_acceptance` would need to
  recompute on the *same* evaluator class, or the refinement/
  acceptance gates need an explicit declared-tolerance policy —
  this decision is the real design work, not the kernel.

## Alternatives ranked by honesty-per-effort

1. **More threads** — `execution.max_threads` is a per-case cap;
   benchmark cases were raised to 8 (this dev box). Free, done.
2. **Cheapest-first + prune widening** — order candidates so coarse
   FAILs prune more of the expensive full plans. No new machinery;
   bounded gain (the coarse phase already prunes).
3. **Hybrid GPU evaluator** — the above. Expected 10–50× on
   field-evaluation wall time for probe-heavy runs; the screening and
   cost layers are negligible compute.
4. **Pure-GPU fixed-order evaluator** — rejected for now: it would
   trade the near-field correctness the model id stands for. If ever
   built, it must be a separate model id with its own reference check.

## When to build it

The trigger is a pilot run where wall time is the demonstrated
blocker — e.g. a customer's real grid exceeding an overnight run —
not speculation. Before then the cheap levers (threads, pruning,
suggested grids) carry the load. The scoping above exists so that
conversation starts from an honest estimate rather than "it'll be
faster on GPU".

## What it must never do

- Change a record's verdict by rounding differently without declaring
  a new model id.
- Weaken the per-probe work limit or the near-field treatment to fit
  the hardware.
- Make the acceptance recomputation verify different arithmetic than
  the search ran, without that split being recorded.
