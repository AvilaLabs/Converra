# Roadmap

Where Converra is going, roughly in priority order. This is a statement
of intent, not a commitment — contributions that move any of these
forward are welcome, and the architecture docs explain where each lands.

## Domain coverage (the real frontier)

- **Wider measured domains.** Embedded datasets currently cover
  20–40 K and up to 8 T. Extending certification reach means binding
  datasets measured at higher fields (~10–20 T) and at 65–77 K — the
  industrial HTS regime. The plumbing is done: any conforming dataset
  bundle can be signed, verified and shipped. The blocker is sourcing
  measured data, not code.
- **More geometry families.** TF/D-shaped coils, solenoids and
  non-planar windings. The `CoilPath`/`CoilPath3D` machinery exists;
  certifying a new family means a kernel, a sampling plan and frozen
  cross-checks, not a rewrite.
- **Critical-state model.** Transport ratios above 1 currently return
  INCONCLUSIVE — a real physics gap that needs the strip model.
- **Deeper mechanical/thermal screens.** Today's Lorentz/hoop checks
  are declared first-order bounds, not structural analysis. Quench
  protection and thermal margins are unmodelled by design until they
  can be modelled honestly.

## Optimization

- **Continuous/structured optimization.** The search is an exhaustive
  grid over discrete choices — deliberately simple and fully recorded.
  A gradient-free or surrogate layer on top is a natural next step.

## Platform

- **Python package on PyPI.** `crates/optcoil-py` already exposes the
  engine (`import converra`); publishing wheels is the remaining step.
- **Vendor dataset pipeline.** The bundle + attestation format is done;
  a documented path for vendors/labs to contribute signed datasets is
  the multiplier for domain coverage.

## What is explicitly not planned

- FEM field solving (Converra imports field maps; it does not compute
  them from mesh). External solvers stay external.
- Silent extrapolation beyond measured domains — that's a property,
  not a gap.
