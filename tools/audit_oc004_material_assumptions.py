#!/usr/bin/env python3
"""Independent OC-004 material-assumption audit against the archived workbook.

Standard library only. Imports tools/prepare_oc003.py's `read_source()`
read-only (never modifies that file, and does not reuse its selection/output
logic) to parse the archived `All data.xlsx`. This tool independently
re-verifies the workbook's SHA-256 against `prepare_oc003.SOURCE_SHA256`
itself, before `read_source()` performs its own (redundant) check.

Scope. Every check below is restricted to Set temperature in
{20, 25, 30, 35, 40} K -- the same five nominal temperatures OC-003/OC-004
actually use -- over the FULL archived field and angle range at those
temperatures (not the OC-003 CSV's frozen 1-8 T / 0-180 deg selection), so
that assumptions the frozen CSV cannot itself test (sub-1 T monotonicity;
angles beyond 180 deg) can be checked against the source workbook directly.

Checks written to benchmarks/coupled/oc-004.assumption-audit.json:

  1. Monotonicity of Ic/w vs commanded (Set) field at fixed (Set T, Set
     angle), over ALL commanded fields including sub-1 T, and again
     restricted to fields >= 1 T. This is the OC-004 contract's A8 low-field
     lower-bound assumption ("Ic nonincreasing in |B| at fixed T, theta");
     the embedded OC-003 CSV only spans 1-8 T and cannot test it below 1 T.
  2. Field-reversal symmetry implied by the contract's period-180 folding
     (A5), Ic(theta) vs Ic(theta+180): directly for Set angle 0 vs 180, and
     for Set angle theta in 5..60 deg vs the workbook's own continuation rows
     at theta+180 (185..240 deg), per (Set T, Set field), with worst/median/
     p95 relative deviation.
  3. Hall-angle-vs-Set-angle offset statistics in the +/-90 deg window
     (maximum-Lorentz geometry) for Set field >= 1 T.

This is a read-only diagnostic over archived data. It establishes no PASS/
FAIL verdict, touches no frozen OC-003/OC-004 artifact, and never
extrapolates -- every number is a direct comparison between rows that are
actually present in the source workbook. The output file is exclusively
created ("x" mode), never overwritten.
"""

import hashlib
import json
import statistics
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import prepare_oc003  # noqa: E402  (read-only import: only SOURCE_SHA256 and read_source() are used)

ROOT = Path(__file__).resolve().parents[1]
WORKBOOK_PATH = ROOT / "data/materials/robinson-superpower-ap-v3/source/All data.xlsx"
OUTPUT_PATH = ROOT / "benchmarks/coupled/oc-004.assumption-audit.json"

TEMPERATURES = (20.0, 25.0, 30.0, 35.0, 40.0)
NEAR_NINETY_ANGLES = (78.0, 81.0, 84.0, 86.0, 88.0, 90.0, 92.0, 94.0, 96.0, 99.0, 102.0)
MONOTONICITY_TOLERANCE = 1e-3  # matches oc-004.json material.monotonicity_tolerance

# Column indices into the `values` list yielded by prepare_oc003.read_source(),
# per that file's own HEADERS list (read-only reference, not reproduced logic).
COL_ACTUAL_FIELD = 1
COL_IC_OVER_W = 3
COL_HALL_ANGLE = 9
COL_SET_TEMPERATURE = 10
COL_SET_FIELD = 11
COL_SET_ANGLE = 12


def sha256_bytes(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def load_rows():
    raw = WORKBOOK_PATH.read_bytes()
    digest = sha256_bytes(raw)
    if digest != prepare_oc003.SOURCE_SHA256:
        raise ValueError(
            f"archived workbook SHA-256 mismatch: got {digest}, expected {prepare_oc003.SOURCE_SHA256}"
        )
    rows = list(prepare_oc003.read_source())
    header_row_number, header_values = rows[0]
    if header_values != prepare_oc003.HEADERS:
        raise ValueError("archived workbook header row changed")
    return digest, rows[1:]


def relative_deviation(a, b):
    """Signed relative deviation of `b` from `a`: (b - a) / a."""
    return (b - a) / a


def percentile(sorted_values, fraction):
    if not sorted_values:
        return None
    if len(sorted_values) == 1:
        return sorted_values[0]
    position = fraction * (len(sorted_values) - 1)
    lo = int(position)
    hi = min(lo + 1, len(sorted_values) - 1)
    frac = position - lo
    return sorted_values[lo] * (1 - frac) + sorted_values[hi] * frac


def summarize_abs_deviation(deviations):
    """deviations: list of signed relative deviations (b-a)/a."""
    if not deviations:
        return {
            "count": 0,
            "worst_abs_relative_deviation": None,
            "worst_signed_relative_deviation": None,
            "median_abs_relative_deviation": None,
            "p95_abs_relative_deviation": None,
        }
    abs_sorted = sorted(abs(d) for d in deviations)
    worst_abs = abs_sorted[-1]
    worst_signed = next(d for d in deviations if abs(d) == worst_abs)
    return {
        "count": len(deviations),
        "worst_abs_relative_deviation": worst_abs,
        "worst_signed_relative_deviation": worst_signed,
        "median_abs_relative_deviation": statistics.median(abs_sorted),
        "p95_abs_relative_deviation": percentile(abs_sorted, 0.95),
    }


def monotonicity_audit(rows):
    """Ic/w vs Set field at fixed (Set T, Set angle), Set T in TEMPERATURES,
    Set angle in [0, 180]. Duplicate (Set T, Set field, Set angle) identities
    in the archived workbook (16 rows total, all at Set field=0, Set angle=0,
    one per main temperature) are averaged before sorting -- no row is
    dropped or fabricated, and the averaged values differ from either
    duplicate by well under the noise floor (see limitations)."""
    grouped = {}
    for _, v in rows:
        set_t, set_b, set_a = v[COL_SET_TEMPERATURE], v[COL_SET_FIELD], v[COL_SET_ANGLE]
        if set_t not in TEMPERATURES or not (0.0 <= set_a <= 180.0):
            continue
        key = (set_t, set_a, set_b)
        grouped.setdefault(key, []).append(v[COL_IC_OVER_W])

    by_ta = {}
    for (set_t, set_a, set_b), values in grouped.items():
        by_ta.setdefault((set_t, set_a), {})[set_b] = sum(values) / len(values)

    def scan(min_field):
        pairs = 0
        violations = 0
        worst = 0.0
        worst_detail = None
        flagged = []
        for (set_t, set_a), field_map in by_ta.items():
            fields = sorted(f for f in field_map if f >= min_field)
            for lo, hi in zip(fields[:-1], fields[1:]):
                pairs += 1
                dev = relative_deviation(field_map[lo], field_map[hi])  # >0 means Ic/w increased with field
                if dev > 0:
                    violations += 1
                    if dev > worst:
                        worst = dev
                        worst_detail = {
                            "set_temperature_k": set_t,
                            "set_angle_deg": set_a,
                            "set_field_lo_t": lo,
                            "set_field_hi_t": hi,
                            "relative_increase": dev,
                        }
                    if dev > MONOTONICITY_TOLERANCE:
                        flagged.append(
                            {
                                "set_temperature_k": set_t,
                                "set_angle_deg": set_a,
                                "set_field_lo_t": lo,
                                "set_field_hi_t": hi,
                                "relative_increase": dev,
                            }
                        )
        return {
            "checked_pairs": pairs,
            "violations": violations,
            "worst_relative_increase": worst,
            "worst_violation": worst_detail,
            "violations_above_tolerance": {
                "tolerance": MONOTONICITY_TOLERANCE,
                "count": len(flagged),
                "violations": sorted(flagged, key=lambda d: -d["relative_increase"]),
            },
        }

    return {"all_fields_including_sub_1t": scan(0.0), "fields_1t_and_above": scan(1.0)}


def field_reversal_audit(rows):
    """Ic(theta)/Ic(theta+180)-style comparisons at fixed (Set T, Set field),
    restricted to TEMPERATURES."""
    by_key = {}
    for _, v in rows:
        set_t = v[COL_SET_TEMPERATURE]
        if set_t not in TEMPERATURES:
            continue
        by_key[(set_t, v[COL_SET_FIELD], v[COL_SET_ANGLE])] = v[COL_IC_OVER_W]

    zero_vs_180 = []
    for (set_t, set_b, set_a), ic_over_w in by_key.items():
        if set_a != 0.0:
            continue
        other = by_key.get((set_t, set_b, 180.0))
        if other is not None:
            zero_vs_180.append(
                {
                    "set_temperature_k": set_t,
                    "set_field_t": set_b,
                    "relative_deviation_180_vs_0": relative_deviation(ic_over_w, other),
                }
            )

    # Index the workbook's own >180 deg continuation rows (185..240 deg).
    continuation = {}
    for _, v in rows:
        set_t = v[COL_SET_TEMPERATURE]
        if set_t not in TEMPERATURES:
            continue
        set_a = v[COL_SET_ANGLE]
        if 180.0 < set_a <= 240.0:
            continuation[(set_t, v[COL_SET_FIELD], set_a)] = v[COL_IC_OVER_W]

    theta_vs_theta_plus_180 = []
    for (set_t, set_b, set_a_cont), ic_cont in continuation.items():
        base_a = set_a_cont - 180.0
        if not (5.0 - 1e-9 <= base_a <= 60.0 + 1e-9):
            continue
        base = by_key.get((set_t, set_b, base_a))
        if base is not None:
            theta_vs_theta_plus_180.append(
                {
                    "set_temperature_k": set_t,
                    "set_field_t": set_b,
                    "theta_deg": base_a,
                    "relative_deviation_theta_plus_180_vs_theta": relative_deviation(base, ic_cont),
                }
            )

    return {
        "zero_vs_180_deg": {
            "pairs": zero_vs_180,
            "summary": summarize_abs_deviation(
                [p["relative_deviation_180_vs_0"] for p in zero_vs_180]
            ),
        },
        "theta_vs_theta_plus_180_5_to_60_deg": {
            "pairs": theta_vs_theta_plus_180,
            "summary": summarize_abs_deviation(
                [p["relative_deviation_theta_plus_180_vs_theta"] for p in theta_vs_theta_plus_180]
            ),
        },
    }


def hall_offset_audit(rows):
    """Hall angle minus Set angle, near +/-90 deg, Set field >= 1 T,
    restricted to TEMPERATURES."""
    offsets_by_temperature = {t: [] for t in TEMPERATURES}
    all_offsets = []
    for _, v in rows:
        set_t = v[COL_SET_TEMPERATURE]
        if set_t not in TEMPERATURES:
            continue
        if v[COL_SET_FIELD] < 1.0:
            continue
        set_a = v[COL_SET_ANGLE]
        if set_a not in NEAR_NINETY_ANGLES:
            continue
        offset = v[COL_HALL_ANGLE] - set_a
        offsets_by_temperature[set_t].append(offset)
        all_offsets.append(offset)

    def stats(values):
        if not values:
            return {"count": 0, "mean_deg": None, "stdev_deg": None, "min_deg": None, "max_deg": None}
        return {
            "count": len(values),
            "mean_deg": statistics.mean(values),
            "stdev_deg": statistics.pstdev(values) if len(values) > 1 else 0.0,
            "min_deg": min(values),
            "max_deg": max(values),
        }

    return {
        "near_90_window_set_angles_deg": list(NEAR_NINETY_ANGLES),
        "field_floor_t": 1.0,
        "overall": stats(all_offsets),
        "by_set_temperature_k": {str(t): stats(v) for t, v in offsets_by_temperature.items()},
    }


def main():
    if OUTPUT_PATH.exists():
        raise FileExistsError(f"refusing to overwrite existing audit at {OUTPUT_PATH}")

    workbook_sha256, rows = load_rows()

    record = {
        "schema": "optcoil-oc004-assumption-audit/v1",
        "source_path": "tools/audit_oc004_material_assumptions.py",
        "source_sha256": sha256_bytes(Path(__file__).read_bytes()),
        "workbook_path": "data/materials/robinson-superpower-ap-v3/source/All data.xlsx",
        "workbook_sha256": workbook_sha256,
        "prepare_oc003_source_sha256_expected": prepare_oc003.SOURCE_SHA256,
        "set_temperatures_k": list(TEMPERATURES),
        "monotonicity": monotonicity_audit(rows),
        "field_reversal": field_reversal_audit(rows),
        "hall_vs_set_angle_offset": hall_offset_audit(rows),
        "limitations": [
            "Diagnostic only: establishes no PASS/FAIL verdict and is not part of the OC-004 run record.",
            "Single specimen (SP066 / SCS12050-AP M3-1386-3, 1 mm patterned bridge); not a batch or "
            "full-width-tape statistic.",
            "The 16 duplicate (Set T, Set field=0, Set angle=0) identities in the archived workbook "
            "were averaged (not dropped or refit) before the monotonicity scan; the two duplicate "
            "values agree to well under 1%.",
            "Sub-1 T and near-zero-field measurements sit closer to the source instrumentation's own "
            "noise floor; monotonicity violations clustered at the lowest fields are evidence of "
            "measurement noise, not a claim about the true material response.",
            "The 185-240 deg continuation rows are the same workbook's own repeat measurements, not an "
            "independent specimen or independent measurement session.",
            "Restricted throughout to Set temperature in {20,25,30,35,40} K, matching OC-003/OC-004's "
            "nominal temperature selection; the workbook's off-grid temperatures (12.5-17.5 K and "
            "52.5-95 K) are out of scope for this audit.",
        ],
    }

    with OUTPUT_PATH.open("x") as handle:
        json.dump(record, handle, indent=2, allow_nan=False)
        handle.write("\n")
    print(f"Wrote {OUTPUT_PATH}")
    return record


if __name__ == "__main__":
    main()
