# Research anchors — external data with provenance

Every external number OptCoil reasoning leans on, with its source and
status. Rules: synthetic values are never presented as measured;
approximate/derived values are marked; anything used in a case or a
commercial claim must carry its provenance here first.

## Conductor pricing (REBCO, 12 mm class)

| Figure | Source | Status |
|--------|--------|--------|
| $30/m | OC-007 ledger | **synthetic placeholder** — never cite as market data |
| ~$125/kA·m → ~$50–75/m at 12 mm (self-field Ic ~400–600 A) | Selvamanickam et al., SuperPower price-performance targets and published $/kA·m figures | **published-industry estimate**, not a supplier quote |
| ~$20M tape per SPARC TFMC coil | 270 km × $75/m | derived estimate, illustrative only |

Commercial claims must bracket this range and state which value was
used; the ledger field stays `price_usd_per_m` with the case's
provenance string carrying the source.

## Real-machine anchors

### SPARC TFMC (MIT/CFS) — arXiv:2308.12301, IEEE TASC

- ~270 km REBCO per coil, 256 turns / 16 pancakes, ~10.4 MA·turns
- 40.5 kA terminal current, 20.1 T peak field, ~10,058 kg
- Peak Lorentz loading ~822 kN/m — the regime where mechanics is the
  binding constraint. OptCoil optima run ~1 kA at ~4 T → ~4 kN/m,
  ~200× under.
- Stack-level load bound used for the v4 mechanical screen:
  ~300–850 kN/m published qualified-conductor limits (declared per
  case as `max_lorentz_load_n_per_m`, assumption not a qualified limit).

### WHAM (UW–Madison / CFS-built REBCO mirror coils)

- 17 T mirror magnets, ~5.5 cm warm bore, 2 kA steady-state, installed
  and operated 2024. Supports small-bore spec plausibility (the
  OC-009 ±5–9 cm cube regime), not a direct racetrack benchmark.

### Feather-M2 (CERN/EuCARD2) — CERN-ACC-NOTE-2017-0045 + IEEE/TASC powering papers

- Aligned-block racetrack dipole: ~800 mm long, ~250 mm straight
  scale, 40 mm clear bore, 2 poles × 2 decks
- Roebel cable: ~15 strands × ~2 mm REBCO (Sunam/SuperOx batch),
  ~6.5 kA shared in parallel
- **Achieved 3.1 T at 5.7 K** standalone (low-performance tape batch);
  5 T was the design target
- Derived ~190 m of 12 mm-equivalent tape — **derived, not quoted**
- OC-010 benchmark anchor. The parallel-cable architecture is what
  motivated schema v4 `strands_parallel`.

## Model-coverage boundaries (honest gaps)

- Material dataset (robinson-superpower-ap-v3-lowfield) validated to
  ~8 T; 5 T-aperture targets would push conductor fields toward the
  boundary — use the 3.1 T achieved spec.
- Tape-edge self-field ratio > 0.1 → INCONCLUSIVE: the gate that bounds
  the small-R corner in OC-010. A screening-current-regime fidelity
  limit, not a capacity failure; strands do not lift it.
- Unmodeled: insulation/fill factors, end-winding geometry, transposition
  losses, joint resistance, quench protection, radiation, detailed
  stress/strain. `mechanical.max_lorentz_load_n_per_m` is a first-order
  screen only.

## Tools landscape (what "existing optimizers" actually are)

- **ROXIE** (CERN-internal): cos-theta accelerator dipoles, field-quality
  objectives. Not a product, not cost-aware.
- **FOCUS / FOCUS-HTS** (Princeton): stellarator coil *shape* for field
  fidelity; HTS variant adds tape-strain/Ic objectives. Not
  cost-under-usable-volume.
- **REGCOIL/STELLOPT**: surface-current → discrete coils, physics
  fidelity again.
- **Bluemira** (UKAEA, open-source): whole-plant framework; magnetics
  library is the OC-011 kernel cross-check implementation — Phase A
  PASS at ~4–5e-4 max |dB|/B across the OC-010 optimum cells. Not on
  PyPI/conda-forge; installed from git (`2.15.1.dev6+ga17002aeb`, Python
  3.11 + numba 0.61; `dolfinx`/FreeCAD/`Part`/`pivy`/`BOPTools` stubbed —
  the `BiotSavartFilament` path never exercises them). System-scale, not
  a per-candidate conductor/cost verifier.
- None publishes verified cost-minimization under a declared usable
  volume with an auditable evidence trail — that is OptCoil's seam.

## Coverage extension candidates (the OC-012 boundary)

The measured dataset tops out at 8.0 T; OC-012 showed 54 of 70
INCONCLUSIVE candidates blocked there (peak conductor fields 9-17 T at
arc apices). Public measured data beyond 8 T exists:

- **UNIGE/Tohoku (TASC 2025, doi:10.1109/tasc.2025.3617455)** — full
  Ic(B,T,theta) characterization of **SuperPower**, Faraday
  Factory/SuperOx and Shanghai tapes: full-width to **19 T** (UNIGE,
  4.2-77 K, five orientations) and patterned microbridges to **24 T**
  (HFLSM, 135 deg angular span). Same manufacturer as the bound
  dataset — the natural extension candidate. Raw-data availability is
  the open question (supplementary or on-request).
- **Senatore et al. scaling law (SUST 29:014002)** — Jc(T,B,theta)
  scaling validated to 19 T across six manufacturers incl. SuperPower;
  3 angles only, but a physics-based *interpolation* path if raw data
  can't be had: fit scaling parameters on the measured 0-8 T domain,
  treat the fitted continuation as a model (with its own provenance
  and an honesty caveat — a scaling-law extrapolation is a model
  claim, not measured data).
- **Wimbush & Strickland public HTS critical-current database** — the
  upstream of our bound dataset. **Checked 2026-09-11** (figshare
  collection 2861821, CC-BY): 10 tape datasets incl. a new SuperPower HM
  entry (Dec 2023); every one tops out at **8 T** — the RRI rig's limit.
  No free path past the boundary.
- **UNIGE/Tohoku availability, checked 2026-09-11**: paper and archive
  entry (unige:189990) carry no raw-data deposit; the tables live with
  the Senatore group. Data request emailed by the project owner.
  Caveats if obtained: 4 mm full-width tape (per-width rescale needed),
  a different SuperPower product line (pinning landscape differs), five
  discrete UNIGE orientations vs our continuous-angle interpolation.
- **Southampton scaling campaign (TASC.2025.3543797)** — SuperPower
  transport to 15 T perpendicular + magnetization to 10 T.

## Interim: labeled model extension (OC-016, in flight)

Until measured data arrives, `robinson-superpower-ap-v3-modelext`
(schema v2, `data_class: measured_with_model_extension`) continues each
measured (T, angle) curve to 20 T by anchored power law with a
conservative margin set by held-out edge checks (~26%; the family
over-predicts the measured 7/8 T edge by <=20.9%). Verdicts against it
are model-informed, never measured-data-verified — its purpose is to
size the coverage-blocked design space, not to certify it.
