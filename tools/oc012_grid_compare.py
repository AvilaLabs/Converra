#!/usr/bin/env python3
"""OC-012 multi-mode grid comparison.

Per (L, R) cell, tabulates the best candidate, cost/savings vs the
800x12 baseline, search status, and the acceptance breakdown for each
run mode present under runs/:

  oc012-startup-dipole            v4  (measured set, no correction)
  oc012-startup-dipole-v5         v5  (+ uniform_transport)
  oc012-startup-dipole-v5-modelext v5mx (+ model-extended dataset)
  oc012-startup-dipole-v6mx       v6mx (+ transverse_bound)
  oc012-startup-dipole-v7mx       v7mx (+ hoop-stress bound)

Usage: python3 tools/oc012_grid_compare.py
"""

import glob
import json
import os

ROOT = os.path.join(os.path.dirname(__file__), "..")
DIRS = [
    ("v4", "oc012-startup-dipole"),
    ("v5", "oc012-startup-dipole-v5"),
    ("v5mx", "oc012-startup-dipole-v5-modelext"),
    ("v6mx", "oc012-startup-dipole-v6mx"),
    ("v7mx", "oc012-startup-dipole-v7mx"),
]


def load(dir_name):
    out = {}
    for path in sorted(glob.glob(os.path.join(ROOT, "runs", dir_name, "run-*.json"))):
        out[os.path.basename(path)[4:-5]] = json.load(open(path))
    return out


def cell_row(rec):
    """(best_geometry, total_usd, savings_pct, search_status, acceptance)."""
    baseline = next(
        (
            c
            for c in rec["candidates"]
            if c["geometry"]["turns_along_normal"] == 800
            and c["geometry"]["tapes_along_width"] == 12
        ),
        None,
    )
    baseline_usd = baseline["cost"]["total_usd"] if baseline else None
    best = None
    if rec.get("best_index") is not None:
        best = rec["candidates"][rec["best_index"]]
    acc = rec.get("acceptance") or {}
    best_acc = (acc.get("best") or {}) if best else {}
    row = {
        "best": None,
        "usd": None,
        "sav": None,
        "search": rec.get("search_status"),
        "accept": acc.get("agreement_status"),
        "refined": best_acc.get("sampling_refinement_status"),
        "mech": best_acc.get("mechanical_agreement_status"),
        "baseline_acc": (acc.get("baseline") or {}).get("agreement_status"),
    }
    if best:
        g = best["geometry"]
        row["best"] = "{}x{}x{}s".format(
            g["turns_along_normal"], g["tapes_along_width"], g["strands_parallel"]
        )
        row["usd"] = best["cost"]["total_usd"]
        if baseline_usd:
            row["sav"] = 100.0 * (baseline_usd - row["usd"]) / baseline_usd
    return row


def main():
    grids = {name: load(d) for name, d in DIRS}
    cells = sorted({c for g in grids.values() for c in g})
    for cell in cells:
        print(f"\n== {cell} ==")
        for name, _ in DIRS:
            rec = grids[name].get(cell)
            if rec is None:
                continue
            r = cell_row(rec)
            best = r["best"] or "-"
            usd = f"${r['usd']:,.0f}" if r["usd"] else "-"
            sav = f"{r['sav']:+.1f}%" if r["sav"] is not None else "-"
            print(
                f"  {name:4} best {best:12} {usd:>10} sav {sav:>7} | "
                f"search {r['search'] or '?':<13} accept {r['accept'] or '?':<5} "
                f"refined {r['refined'] or '-':<5} mech {r['mech'] or '-':<5} "
                f"baseline {r['baseline_acc'] or '-'}"
            )


if __name__ == "__main__":
    main()
