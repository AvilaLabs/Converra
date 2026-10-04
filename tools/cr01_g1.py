#!/usr/bin/env python3
"""CR-01 gate G1: synthetic recovery on each product's real node layout.

Usage: cr01_g1.py OUTPUT.json
Real Ic values are never used: only node coordinates are kept.
"""
import json
import math
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import cr01_run as R  # noqa: E402

TRUE = {"c0": 24.0, "Tstar": 38.0, "alpha": 0.8}
TOL = 1e-9


def synth(coords):
    return {k: math.exp(TRUE["c0"] - k[0] / TRUE["Tstar"] - TRUE["alpha"] * math.log(k[1])) for k in coords}


def rel(a, b):
    return abs(a - b) / abs(b)


def check_scenario(coords_nodes, scen):
    res = R.model_scenario(synth(coords_nodes), scen)
    out = {"verdict": res["verdict"], "n_train": res["n_train"], "n_targets": res["n_targets"]}
    if "M1" not in res:
        out["pass"] = None
        out["reason"] = res.get("reason")
        return out
    m = res["M1"]
    perr = max(rel(m.get(k, float("nan")), TRUE[k]) for k in TRUE) if not m["degenerate"] else float("inf")
    terr = max(abs(r["e"]) for r in m["predictions"]) if not m["degenerate"] else float("inf")
    out.update(max_param_rel_err=perr, max_target_abs_rel_err=terr, pass_=bool(perr <= TOL and terr <= TOL))
    out["pass"] = out.pop("pass_")
    return out


def main(out):
    sources = json.loads((R.DATA / "sources.json").read_text())["sources"]
    prods = {}
    allpass = True
    for s in sorted(x["slug"] for x in sources):
        perp, _, _ = R.read_nodes(s)  # coordinates only
        coords = [k for k in perp if k[1] > 0]  # B=0 nodes are never training or target nodes
        prods[s] = {}
        for sc in ("S1", "S2"):
            r = check_scenario(coords, sc)
            prods[s][sc] = r
            if r["pass"] is False:
                allpass = False
    # degenerate input: constant T for the training nodes
    cn = [(77.5, b) for b in (0.5, 0.7, 1, 1.5, 2, 3)] + [(30.0, 3.0), (20.0, 5.0), (25.0, 7.0)]
    d = R.model_scenario(synth(cn), "S1")
    degenerate = {"verdict": d["verdict"], "n_train": d["n_train"], "pass": d["verdict"] == "INCONCLUSIVE"}
    # missing anchor
    ma_nodes = [(t, b) for t in (65.0, 70.0, 77.5) for b in (0.5, 1, 2, 3)] + [(20.0, 5.0), (25.0, 7.0)]
    m = R.model_scenario(synth(ma_nodes), "S2")
    missing = {"verdict": m["verdict"], "pass": m["verdict"] == "NOT_EVALUATED"}
    gates = {"degenerate_constant_T": degenerate, "missing_anchor": missing}
    allpass = allpass and degenerate["pass"] and missing["pass"]
    res = {"gate": "G1_synthetic_recovery", "true_parameters": TRUE, "tolerance": TOL,
           "runner_sha256": R.sha256(R.__file__), "g1_script_sha256": R.sha256(__file__),
           "protocol_sha256": R.sha256(R.PROTOCOL), "products": prods, "special_cases": gates,
           "G1": "PASS" if allpass else "FAIL"}
    Path(out).parent.mkdir(parents=True, exist_ok=True)
    Path(out).write_text(json.dumps(res, indent=1))
    print("G1", res["G1"])


if __name__ == "__main__":
    main(sys.argv[1])
