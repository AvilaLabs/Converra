# External review brief — OC-023-seam

For an external magnet engineer challenging one end-to-end result.
Read alongside `docs/OC023.md` (diagnosis) and `docs/OC029.md` (lever
decomposition). Source review only requires the repo; reproducing the
numbers requires `cargo run -p optcoil-cli --release -- coupled-search
benchmarks/coupled/oc-023-seam.json` (~18 min, 4 threads).

## What the artifact claims — and does not

`runs/oc-023-seam.json` records a coupled search over 54 pack/assignment
candidates for a 0.9 T warm-bore insert requirement (racetrack, fixed
0.20 m bend / 0.30 m straight-half-length):

- baseline: 240×3 pack, uniform SuperPower binding — $60,269.70 modeled
- optimum: **240×2 pack, both radial regions on `hts-theva`** —
  $50,489.97 modeled (16.23%)
- `search_status PASS`, `acceptance agreement PASS`, acceptance checker
  recomputed cost/field/screens/refinement independently (agreement
  `0.000e0` on both recompute legs)
- limiting point: `a0` station, tape 2, turn 1, width index 3 —
  screening allowance 1138.8 A vs operating 909.0 A, max utilization
  0.798 against the declared 0.8 limit — **the design sits at 99.8% of
  its own declared screening margin.**

What it does not claim: this is a modeled, screening-only optimum under
declared assumptions with invented prices. `conductor_qualification`
is INCONCLUSIVE and `engineering_status` NOT_EVALUATED by construction.
No mechanical, thermal, quench, or manufacturing acceptance is implied.
The record's own `limitations` block lists the unverified assumptions,
now including per-binding width applicability with numbers.

## The declared-assumption inventory to challenge

Every item below is declared in `record.case` — each is an assumption a
reviewer can accept, bound, or reject; none is silently applied.

| Assumption | Where declared | Most likely failure mode |
|---|---|---|
| `period_180_field_reversal_seam_stitched` on the THEVA binding | `case.tape_specs.hts-theva.material.angle_mapping` | If the real tape's Ic(θ) is not period-180-symmetric at this precision (asymmetric pinning), the seam blend overestimates Ic near θ≈0/180 — **the limiting point is near this direction** |
| `minimum_of_mirror_pair` | `material.mirror_policy` | Mirror asymmetry beyond the declared nonzero amount |
| `pack_field_as_applied_field_self_field_consistent` | `material.field_basis_mapping` | The bridge law already contains the specimen's self-field response; adding pack self-field could double-count |
| `total_magnitude_with_transverse_angle` | `material.field_magnitude_policy` | Ic depends on field angle, not magnitude — a different decomposition gives a different θ |
| `monotone_field_lower_bound` + `low_field_clamp_t` | `material.low_field_policy` | Below 1 T the dataset does not verify monotonicity |
| Critical-state strip correction | `mechanical`/`material` blocks | First-order correction, not a Bean/FEA model |
| Width applicability | record `limitations` (new) | THEVA specimen is a 0.5 mm bridge from 6 mm tape applied at 12 mm winding width — bridge-to-full-width transfer unverified; same for Shanghai 0.5 mm from 4 mm |
| Prices | `case.tape_specs.*.price_usd_per_m` | Invented placeholders — the 16.2% figure moves with any real quote |

## The three questions we most want challenged

1. **Is the limiting mechanism real?** The optimum is a 2-tape pack at
   util 0.798 vs the declared 0.8 — a 0.2% margin. Any change to the
   Ic model, the angle convention, or the self-field estimate that
   shifts the limiting allowance by >0.2% flips the verdict. Which
   declared assumption would you attack first, and what independent
   estimate (back-of-envelope or your own solver) would bound it?

2. **Is the seam contract defensible here?** The binding declares
   period-180 symmetry, and the interpolator then bridges the 0.04°
   measured gap at the fold with a convex blend of measured points on
   both sides (never extrapolated). For THEVA's measured anisotropy,
   does the fold blend err toward or away from the true Ic at θ≈0 —
   and by enough to matter against a 0.2% margin?

3. **Is the baseline honest?** The comparison claims 16.2% vs a
   240×3 baseline already at util 0.67. Is that a competent engineer's
   uniform starting design, or does the savings partly reflect a
   weak baseline? The OC-029 lever study (same substrate) found sizing
   alone buys 0% — consistent with competence — but an outside read
   is the point of this item.

## What acceptance does and does not establish

`search_acceptance` recomputes the optimum's cost ledger, field at the
requirement probe, screening statuses, the §9.4 refined sampling plan,
and every declared screen on its own rerun — **it shares the field
kernel with the search.** Agreement therefore verifies arithmetic and
resampling density, not the kernel's physics. The partial internal
answer: `kernel_crosscheck` (OC-011) evaluates a frozen probe deck
through `RacetrackEvaluator` and an independently implemented evaluator;
the shared assumptions still bind both. A reviewer running a genuinely
separate Biot–Savart or full pack solve on the *limiting* station would
close that hole — that is the single highest-value external check.

## Artifacts

- case: `benchmarks/coupled/oc-023-seam.json`
- record: `runs/oc-023-seam.json` (regenerate with the command above)
- verification: `optcoil verify runs/oc-023-seam.json`
  (8 PASS / 2 NOT_CHECKED — artifact integrity, not physics)
- dataset bundles: `data/materials/robinson-theva-ap-v2`,
  `robinson-shanghai-hflt-v3`, `robinson-superpower-ap-v3` — each
  carries its own `limitations`, normalization, and specimen metadata
- assumptions doc: `docs/OC023.md` §"Resolution"
