#!/usr/bin/env python3
"""OC-024: field parity against the NHMFL 32 T all-superconducting magnet.

Reads the two run records produced by `optcoil coupled-search` on
benchmarks/coupled/oc-024a.json (inner REBCO coil) and oc-024b.json
(outer REBCO coil), takes each pack's recorded
`unit_bore_bz_t_per_ampere_turn`, and predicts the insert's center-field
contribution at the published operating currents. The comparison target
is the published 17 T the HTS insert contributes (the LTS outsert
provides the other 15 T of the 32 T total).

Turn counts are derived two ways from published figures — tape length
wound per coil, and the published engineering current density Jave —
because the primary sources do not tabulate turns directly; the parity
is reported as the band between them, not a point.

Usage: python3 tools/oc024_32t_parity.py runs/oc024/inner.json runs/oc024/outer.json
"""

import json
import math
import sys

IOP_A = [173.0, 180.0]  # commissioned operating current; original spec 180 A
PUBLISHED_HTS_T = 17.0  # REBCO insert's bore contribution (of 32 T total)

# Published geometry (Weijers et al., NHMFL 32 T magnet technology;
# IEEE TASC 22(3) 4300704): IR/OR/height, double-pancake module count,
# total tape length wound, and engineering current density Jave per coil.
COILS = {
    "inner": {"ir_mm": 20.0, "or_mm": 70.0, "h_mm": 178.0, "dp_modules": 20,
              "tape_km": 2.8, "jave_a_mm2": 176.0},
    "outer": {"ir_mm": 82.0, "or_mm": 116.0, "h_mm": 320.0, "dp_modules": 36,
              "tape_km": 6.6, "jave_a_mm2": 196.0},
}


def turns_by_tape_length(c):
    pancakes = 2 * c["dp_modules"]
    per_pancake_m = c["tape_km"] * 1e3 / pancakes
    r_mean = (c["ir_mm"] + c["or_mm"]) / 2e3
    return round(per_pancake_m / (2 * math.pi * r_mean)) * pancakes


def turns_by_jave(c, iop):
    area_mm2 = (c["or_mm"] - c["ir_mm"]) * c["h_mm"]
    return c["jave_a_mm2"] * area_mm2 / iop


def main():
    inner, outer = (json.load(open(p)) for p in sys.argv[1:3])
    b1 = inner["candidates"][0]["unit_bore_bz_t_per_ampere_turn"]
    b2 = outer["candidates"][0]["unit_bore_bz_t_per_ampere_turn"]
    print(f"unit bore field per A-turn: inner {b1:.4e} T, outer {b2:.4e} T")
    print()
    for iop in IOP_A:
        for label, n1, n2 in (
            ("tape length", turns_by_tape_length(COILS["inner"]),
             turns_by_tape_length(COILS["outer"])),
            ("published Jave", turns_by_jave(COILS["inner"], iop),
             turns_by_jave(COILS["outer"], iop)),
        ):
            b = (b1 * n1 + b2 * n2) * iop
            print(f"NI basis {label:<14} Iop {int(iop)} A: "
                  f"N = {n1:.0f} + {n2:.0f} turns -> "
                  f"predicted {b:6.2f} T vs published {PUBLISHED_HTS_T} T "
                  f"({(b / PUBLISHED_HTS_T - 1) * 100:+.1f}%)")
        print()


if __name__ == "__main__":
    main()
