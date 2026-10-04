#!/usr/bin/env python3
"""CR-01 v2 gate G1: synthetic recovery on each product's real node layout.

Usage: cr01v2_g1.py OUTPUT.json
Real Ic values are never used: only node coordinates are kept.
"""
import json
import math
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import cr01_run as R  # noqa: E402
import cr01v2_run as V  # noqa: E402

TRUE = {"c0": 24.0, "Tstar": 38.0, "alpha": 0.8}
TOL = 1e-9


def synth(coords):
    return {k: math.exp(TRUE["c0"] - k[0] / TRUE["Tstar"] - TRUE["alpha"] * math.log(k[1])) for k in coords}


def rel(a, b):
    return abs(a - b) / abs(b)


def check_model(coords, scen):
    res = V.model_scenario(synth(coords), scen)
    out = {"verdict": res["verdict"], "n_train": res["n_train"], "n_targets": res["n_targets"]}
    if "M1" not in res:
        out.update(reason=res.get("reason"), **{"pass": None})
        return out
    m = res["M1"]
    perr = max(rel(m.get(k, float("nan")), TRUE[k]) for k in TRUE) if not m["degenerate"] else float("inf")
    terr = max(abs(r["e"]) for r in m["predictions"]) if not m["degenerate"] else float("inf")
    out.update(max_param_rel_err=perr, max_target_abs_rel_err=terr, **{"pass": bool(perr <= TOL and terr <= TOL)})
    return out


def common(k):
    """One common map; products are fixed distinct multiples of it."""
    return math.exp(23.0 - k[0] / 31.0 - 1.3 * math.log(k[1]) + 0.01 * k[0] * math.log(k[1]))


def check_lift(coords_all, headline, scen):
    mult = {s: 0.5 + 0.37 * i for i, s in enumerate(sorted(coords_all))}
    perp_all = {s: {k: mult[s] * common(k) for k in coords_all[s]} for s in coords_all}
    out, ok = {}, True
    for s in sorted(coords_all):
        r = V.lift_base(s, scen, perp_all, headline)
        if "predictions" not in r:
            out[s] = {"verdict": r["verdict"], "pass": None}
            continue
        err = max(abs(p["e"]) for p in r["predictions"])
        p = bool(err <= TOL)
        ok = ok and p
        out[s] = {"verdict": r["verdict"], "n_targets": r["n_targets"], "max_target_abs_rel_err": err, "pass": p}
    return out, ok


def main(out):
    sources = json.loads((R.DATA / "sources.json").read_text())["sources"]
    slugs = sorted(x["slug"] for x in sources)
    headline = sorted(x["slug"] for x in sources if not x["not_a_tape"])
    prods, coords_all = {}, {}
    allpass = True
    for s in slugs:
        perp, _, _ = R.read_nodes(s)  # coordinates only
        coords = [k for k in perp if k[1] > 0]
        coords_all[s] = coords
        prods[s] = {}
        for sc in ("S3", "S4"):
            r = check_model(coords, sc)
            prods[s][sc] = r
            if r["pass"] is False:
                allpass = False
    lifts = {}
    for sc in ("S5", "S6"):
        lifts[sc], ok = check_lift(coords_all, headline, sc)
        allpass = allpass and ok
    res = {"gate": "G1_synthetic_recovery_v2", "true_parameters": TRUE, "tolerance": TOL,
           "runner_sha256": R.sha256(V.__file__), "v1_runner_sha256": R.sha256(R.__file__),
           "g1_script_sha256": R.sha256(__file__), "protocol_sha256": R.sha256(V.PROTOCOL),
           "products": prods, "lift_populations": lifts, "G1": "PASS" if allpass else "FAIL"}
    Path(out).parent.mkdir(parents=True, exist_ok=True)
    Path(out).write_text(json.dumps(res, indent=1))
    print("G1", res["G1"])


if __name__ == "__main__":
    main(sys.argv[1])
