#!/usr/bin/env python3
"""OC-012 v4-vs-v5 comparison table.

Tabulates, per (L, R) cell, the best verified candidate under schema v4
(no self-field correction) and schema v5 (uniform_transport correction),
from the run records in runs/oc012-startup-dipole{,-v5}/. Also lists the
v4 INCONCLUSIVE population with an estimated transport-dominance ratio so
the flip verdict can be checked candidate-by-candidate after the v5 run.

Usage: python3 tools/oc012_compare.py
"""

import glob
import json
import os

ROOT = os.path.join(os.path.dirname(__file__), "..")
DIRS = {
    "v4": os.path.join(ROOT, "runs/oc012-startup-dipole"),
    "v5": os.path.join(ROOT, "runs/oc012-startup-dipole-v5"),
}
OVERPREDICTION_BUDGET = 0.1  # case-declared; K_carried/k_used = (1-b)*util


def load(dir_path):
    records = {}
    for path in sorted(glob.glob(os.path.join(dir_path, "run-*.json"))):
        cell = os.path.basename(path)[4:-5]
        records[cell] = json.load(open(path))
    return records


def cell_summary(record):
    best = None
    if record.get("best_index") is not None:
        best = record["candidates"][record["best_index"]]
    inconclusive = [
        c for c in record["candidates"] if c.get("status") == "INCONCLUSIVE"
    ]
    return best, inconclusive


def fmt_candidate(candidate):
    if candidate is None:
        return "-"
    g = candidate["geometry"]
    cost = candidate.get("cost") or {}
    return "{}x{}x{}s {:.0f} m ${:.0f}".format(
        g["turns_along_normal"],
        g["tapes_along_width"],
        g["strands_parallel"],
        cost.get("installed_length_m", float("nan")),
        cost.get("total_usd", float("nan")),
    )


def main():
    v4, v5 = load(DIRS["v4"]), load(DIRS["v5"])
    cells = sorted(set(v4) | set(v5))
    print("=== OC-012 grid: best verified candidate per cell ===")
    print(f'{"cell":>16} | {"v4 best":<34} | {"v5 best":<34} | v5 verdict')
    for cell in cells:
        b4, _ = cell_summary(v4[cell]) if cell in v4 else (None, [])
        b5, _ = cell_summary(v5[cell]) if cell in v5 else (None, [])
        verdict = ""
        if b4 is None and b5 is not None:
            verdict = "v5 RESOLVED an optimum v4 could not certify"
        elif b4 is not None and b5 is not None:
            c4 = b4["cost"]["total_usd"]
            c5 = b5["cost"]["total_usd"]
            verdict = "same" if abs(c4 - c5) < 1e-6 else f"v5 {c5-c4:+.0f} USD"
        print(
            f"{cell:>16} | {fmt_candidate(b4):<34} | {fmt_candidate(b5):<34} | {verdict}"
        )

    print("\n=== v4 INCONCLUSIVE candidates (est. transport ratio) ===")
    print(
        f'{"cell":>16} {"geom":>12} {"k_used_max":>10} {"util":>6} {"est.transp":>10}'
    )
    for cell, record in v4.items():
        _, inconclusive = cell_summary(record)
        for c in inconclusive:
            g = c["geometry"]
            scr = c.get("screening") or {}
            kmax, util = scr.get("max_self_field_ratio"), scr.get("max_utilization")
            if kmax is None or util is None:
                continue
            est = kmax * (1 - OVERPREDICTION_BUDGET) * util
            print(
                f'{cell:>16} {g["turns_along_normal"]}x{g["tapes_along_width"]}x'
                f'{g["strands_parallel"]:>4} {kmax:>10.2f} {util:>6.2f} {est:>10.2f}'
            )

    # v5 transition + blocker table: for each candidate INCONCLUSIVE under
    # v4, what did it become under v5 and what physical boundary holds it.
    print("\n=== v4 INCONCLUSIVE -> v5 transitions ===")
    print(
        f'{"cell":>16} {"geom":>10} {"v4":>14} {"v5":>14} '
        f'{"transp":>7} {"unsup":>6} {"alongI":>6}  v5 blocker'
    )
    totals = {}
    for cell, r4 in v4.items():
        r5 = v5.get(cell)
        if r5 is None:
            continue
        m5 = {
            (c["geometry"]["turns_along_normal"], c["geometry"]["tapes_along_width"]): c
            for c in r5["candidates"]
        }
        for c4 in r4["candidates"]:
            if c4["status"] != "INCONCLUSIVE":
                continue
            g = c4["geometry"]
            key = (g["turns_along_normal"], g["tapes_along_width"])
            c5 = m5.get(key)
            if c5 is None:
                continue
            s5 = c5.get("screening") or {}
            pc = s5.get("point_counts") or {}
            unsup = pc.get("unsupported") or 0
            along = pc.get("along_current_excluded") or 0
            transp = s5.get("max_transport_self_field_ratio")
            if c5["status"] != "INCONCLUSIVE":
                blocker = "RESOLVED -> " + c5["status"]
            elif unsup > 0:
                blocker = "coverage (points outside measured domain)"
            elif along > 0:
                blocker = "along-current field fraction > limit"
            elif transp is not None and transp > 1.0:
                blocker = "critical-state regime (Phase 2)"
            else:
                blocker = "inconclusive (other)"
            totals[blocker] = totals.get(blocker, 0) + 1
            print(
                f'{cell:>16} {key[0]}x{key[1]:<4} {c4["status"]:>14} {c5["status"]:>14} '
                f'{transp if transp is not None else float("nan"):>7.2f} '
                f'{unsup:>6} {along:>6}  {blocker}'
            )
    if totals:
        print("\nv5 INCONCLUSIVE decomposition:", totals)


if __name__ == "__main__":
    main()
