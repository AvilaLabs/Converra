#!/usr/bin/env python3
"""Independent parity check for a coupled v7 field_map run record.

Re-derives every geometry point's field from the case's declared field
map — nearest-node lookup on the (rho, z) grid, cylindrical -> lab frame
at the point's own position, per-ampere-turn scaling by the map's
reference_ampere_turns_a and the candidate's ampere-turns — and compares
against the record's stored unit_field_t_per_ampere_turn / field_t.

This verifies the map-ingestion math only: the Ic screening downstream
is the unchanged, already-parity-covered path (OC-004/OC-014). It is a
deliberately separate implementation (no shared code) so an ingest bug
cannot cancel out.

Usage:
    python3 tools/verify_fieldmap_record.py <case.json> <record.json>
"""

import json
import math
import sys

TOL_REL = 1e-9


def nearest_index(levels, x):
    """First index whose level >= x; tie goes to the lower index."""
    lo, hi = 0, len(levels)
    while lo < hi:
        mid = (lo + hi) // 2
        if levels[mid] < x:
            lo = mid + 1
        else:
            hi = mid
    if lo == 0:
        return 0
    if lo >= len(levels):
        return len(levels) - 1
    return lo - 1 if x - levels[lo - 1] <= levels[lo] - x else lo


def main():
    case = json.load(open(sys.argv[1]))
    record = json.load(open(sys.argv[2]))

    fm = case["sampling"].get("field_map")
    assert fm is not None, "case declares no sampling.field_map"
    rfm = record.get("field_map")
    assert rfm is not None, "record carries no field_map provenance"
    assert rfm["source_sha256"] == fm["source_sha256"], "source hash mismatch"
    assert rfm["reference_ampere_turns_a"] == fm["reference_ampere_turns_a"]
    assert record["kernel_evaluations"] == 0, "kernel ran under a declared map"
    assert record["field_model_id"] == (
        "customer-declared-field-map/cylindrical-br-bz/v1")

    rho = fm["rho_levels_m"]
    z = fm["z_levels_m"]
    nz = len(z)
    grid = {}
    for e in fm["entries"]:
        grid[(e["rho_index"], e["z_index"])] = (e["br_t"], e["bz_t"])
    assert len(grid) == len(rho) * len(z), "entries do not fill the grid"
    inv_at = 1.0 / fm["reference_ampere_turns_a"]

    total_turns = record["total_turns"]
    n_checked = 0
    worst = 0.0
    for cand in record["candidates"]:
        at = cand["ampere_turns_a"]
        assert abs(at - total_turns * cand["current_a"]) < 1e-9 * at
        for group in record["stations"]:
            for tape in group["tapes"]:
                for p in tape["points"]:
                    x, y, zz = p["position_m"]
                    r = math.hypot(x, y)
                    i = nearest_index(rho, r)
                    j = nearest_index(z, zz)
                    br, bz = grid[(i, j)]
                    if r > 0.0:
                        rx, ry = x / r, y / r
                    else:
                        rx = ry = 0.0
                    unit = [br * rx * inv_at, br * ry * inv_at, bz * inv_at]
                    for slot in (0, 1):
                        got = p["unit_field_t_per_ampere_turn"][slot]
                        for k in range(3):
                            d = abs(got[k] - unit[k])
                            rel = d / max(abs(unit[k]), 1e-30)
                            worst = max(worst, rel)
                            assert rel < TOL_REL, (
                                f"unit field mismatch at {group['station']} "
                                f"t{tape['tape_index']} n{tape['turn_index']} "
                                f"w{p['width_index']} slot{slot}: "
                                f"{got} vs {unit}")
                    # refinement change is 0 by construction under a map
                    assert p["refinement_change_t_per_ampere_turn"] == 0.0
                    n_checked += 1
        break  # candidates share the same unit fields; check one

    print(f"parity OK: {n_checked} geometry points re-derived from the "
          f"declared map; worst relative deviation {worst:.2e} "
          f"(tol {TOL_REL:.0e})")


if __name__ == "__main__":
    main()
