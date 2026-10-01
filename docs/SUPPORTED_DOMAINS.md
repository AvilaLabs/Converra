# Supported domains — Converra screening boundaries

For the current user workflow, start with the [Converra Handbook](https://converra.avilalabs.org/docs/models.html). This reference describes declared screening boundaries; a screening pass does not establish engineering qualification.

The honest spec sheet. Every boundary below is enforced by a verdict,
not a disclaimer: candidates outside a supported domain come back
INCONCLUSIVE with the specific boundary named — never silently
extrapolated, never a soft fail.

## Geometry

| Shape | Status |
|---|---|
| Racetrack (straight + semicircular arcs) | **Supported** — primary family |
| Circular coil | **Supported** — the L→0 limit; kernel-validated against the closed-form loop (OC-011 self-test: 2.09446 vs 2.09440 T) |
| Planar piecewise line-and-arc paths | **Supported** — declared planar path and sampling contract |
| Non-planar helix paths | **Declared-map screening supported** — `pack.path3d` requires a compatible Cartesian field map; see [OC-031](OC031.md) |
| General CAD / arbitrary non-planar solids | **Not supported** as an automatic built-in field model |

## Material (embedded measured datasets)

Five measured datasets share the declared 20–40 K × 0–180° window;
per-dataset field floors differ:

| Dataset | Tape | Field domain | Notes |
|---|---|---|---|
| `robinson-superpower-ap-v3` (+ `-lowfield`) | SuperPower AP | 0.05 – **8.0 T** | primary; lowfield extension fills 0.05–0.7 T |
| `robinson-shanghai-hflt-v3` | Shanghai SC HFLT | 1.0 – 8.0 T | same instrument + window |
| `robinson-theva-ap-v2` | THEVA Pro-Line AP | 1.0 – 8.0 T | denser 105–135° wedge |
| `robinson-ffj-ybco-v1` | Faraday Factory Japan YBCO | **0.01 – 8.0 T** | densest low-field decade (0.01–0.7 T) + near-peak angle mesh |

| Axis | Domain | Outside domain → |
|---|---|---|
| Applied field | per-dataset floor – **8.0 T** | `unsupported` points → INCONCLUSIVE |
| Temperature | 19.98 – 40.0 K | `unsupported` → INCONCLUSIVE |
| Field angle (normal-plane) | full 0–180° fold | covered |
| Along-current field fraction | ≤ **0.20** of local magnitude | `along_current_excluded` → INCONCLUSIVE |
| — with `transverse_bound` (schema v6/v3, opt-in) | 0.20 < fraction ≤ **0.50** | queried at full magnitude + transverse angle, labeled `along_current_bounded` — a bounded estimate, not a clean measured-plane one; > 0.50 stays `along_current_excluded` |

Practical consequence (measured on OC-012): a 5 T-bore dipole spec
drives 9–17 T peak conductor fields at tight arc apices — designs in
that regime cannot be certified on any embedded dataset. Certification
reach extends the moment a dataset measured to ~10–17 T is bound.

**Model extension (sensitivity only):** `robinson-superpower-ap-v3-modelext`
continues the measured curves to 20 T by anchored power law with a
conservative margin (OC-016). It exists to size the coverage-blocked
design space; any verdict depending on a modeled node is model-informed,
not measured-data-verified — the `data_class` label and the `modelext`
dataset id make that distinction part of every bound record.

**Published model fits (sizing/exploration only):** `babouche-*-memfit-*`
datasets evaluate the published maximum-entropy-model parametrization of
Babouche et al. 2026 (doi:10.1088/1361-6668/ae940e, CC BY 4.0) on a
declared interior grid — every row is a model evaluation, none a
measurement (`data_class: published_model_fit`). Domain is the published
parameter quad in (T,B): T ≤ 35 K in the two-Lorentzian+Gaussian regime,
fields up to the quad's 19 T corner — the `-v2` ids emit nodes to
17–19 T near 20 K where the quad covers them (19 T exists only at
20 K exactly); `-v1` ids keep the original ≤16 T grid. These datasets
quantify which candidates *would* pass if the published fit held —
verdicts are model-informed, never measured-data-verified.

## Self-field regime

| Case | Handling |
|---|---|
| transport ratio ≤ 1 (schema v5+, `uniform_transport`) | corrected query `|B_applied| + μ0K/2`; determined verdicts |
| transport ratio > 1 | INCONCLUSIVE — critical-state redistribution is real physics, not a bound; pending the OC-014 Phase-2 strip model |
| schema ≤ v4 | crude `max_self_field_ratio ≤ 0.1` gate at the limiting point |

## Mechanical

Two first-order screens, both declared assumptions rather than
qualified limits:

- Lorentz load (schema v4+): `I_op · B_peak` in N/m vs
  `mechanical.max_lorentz_load_n_per_m` — OC-013 uses the published
  300 kN/m stack-level figure.
- Hoop stress (schema v7+, OC-018): per-strand
  `(I_op/s)·B_peak·R_outer / tension_section_area_m2` vs
  `mechanical.max_hoop_stress_pa` — the pressure-vessel bound
  `T = f·R`, with the load path declared explicitly because pack fill
  and support structure are unmodeled.

Both are screens, not a structural analysis: no stress concentration,
no fatigue, no delamination component, no quench protection, no
thermal/hydraulic analysis.

## Verification semantics

- `PASS` — every gate passed at the declared sampling plan.
- `FAIL` — a gate failed (requirement, capacity, geometry, mechanical).
- `INCONCLUSIVE` — a supported-domain or fidelity boundary was hit; the
  record names which one, per candidate.
- `NOT_EVALUATED` — never evaluated (e.g. pruned without evaluation is
  still reported, marked).

Independent acceptance (`optcoil-search::acceptance`) recomputes cost,
field and screening for the optimum and baseline, and applies the
contract §9.4 refined-sampling gate. `search_status`/`agreement_status`
are campaign-level verdicts and fail closed on any unverifiable leg —
read per-candidate statuses for design decisions.

## Economics

Cost is a model: `price_usd_per_m × installed metres × (1+scrap)` plus
assembly and joint costs. Sourced price band exists
($50–75/m for 12-mm-class REBCO, docs/RESEARCH_ANCHORS.md); it is a
published-industry estimate, not a supplier quote. Dollar outputs are
as defensible as the price bound they carry.
