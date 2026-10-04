#!/usr/bin/env python3
"""CR-01 runner for protocol v2: implements benchmarks/cr01/protocol-v2.json literally.

Usage: cr01v2_run.py OUTPUT.json
Reuses the unchanged node, fit, metric and v1 S0 helpers from cr01_run.py.
"""
import json
import sys
from pathlib import Path

import numpy as np

sys.path.insert(0, str(Path(__file__).resolve().parent))
import cr01_run as R  # noqa: E402

ROOT = R.ROOT
PROTOCOL = ROOT / "benchmarks/cr01/protocol-v2.json"
PROTOCOL_SHA = "73f0f67df7adb70d53c39622f90f7026a3983bc1d265729ce5a5ee3468b9b011"
SCEN_NODES = {
    "S3": ([20.0, 30.0, 40.0], [2.0, 3.0]),
    "S4": ([20.0, 40.0], [3.0, 5.0]),
}
BASES = {"S5": (70.0, 3.0), "S6": (77.5, 1.0)}
SCENARIOS = ("S3", "S4", "S5", "S6")


def fit(train, form):
    """R.fit with the coefficients from a QR/SVD least-squares solve instead of the normal equations.

    The M2 design is ill-conditioned (a1 ~ 1e-5); the normal-equation solve in R.fit differs from
    the lstsq solution by ~1e-9 relative, which attempt 01 of the v2 checker flagged.
    """
    f = R.fit(train, form)
    if f["degenerate"] and "beta" not in f:
        return f
    keys = sorted(train)
    X = R.design(keys, form)
    y = np.array([R.math.log(train[k]) for k in keys])
    beta = np.linalg.lstsq(X, y, rcond=None)[0]
    res = y - X @ beta
    f["beta"] = [float(v) for v in beta]
    f["s2"] = float(res @ res) / f["dof"] if f["dof"] > 0 else float("nan")
    if not f["degenerate"]:
        f["c0"] = float(beta[0])
        f["Tstar"] = float(-1.0 / beta[1])
        if form == "M1":
            f["alpha"] = float(-beta[2])
        else:
            f["a0"], f["a1"] = float(-beta[2]), float(-beta[3])
    return f


def training_nodes(scen):
    ts, bs = SCEN_NODES[scen]
    return [(t, b) for t in ts for b in bs]


def model_scenario(perp, scen):
    """S3/S4 for one product from its perpendicular node dict."""
    need = training_nodes(scen)
    train = {k: perp[k] for k in need if k in perp}
    tgt = [k for k in R.targets_of(perp) if k not in need]
    res = {"scenario": scen, "n_train": len(train), "n_targets": len(tgt),
           "training_nodes": R.fmt_nodes(train), "target_nodes": R.fmt_nodes({k: perp[k] for k in tgt})}
    if len(train) < len(need):
        res.update(verdict="NOT_EVALUATED", reason="missing required training node")
        return res
    if not tgt:
        res.update(verdict="NOT_EVALUATED", reason="no target nodes")
        return res
    forms = ("M1", "M2") if scen == "S3" else ("M1",)
    for form in forms:
        f = fit(train, form)
        entry = {k: v for k, v in f.items() if k not in ("XtXi", "form_p")}
        if f["degenerate"]:
            entry["verdict"] = "INCONCLUSIVE"
            res[form] = entry
            continue
        pr = R.predict(f, tgt, form, interval=(form == "M1"))
        rows = []
        for k in tgt:
            meas = perp[k]
            d = {"T": k[0], "B": k[1], "measured": meas, **pr[k]}
            d["e"] = d["pred"] / meas - 1.0
            if form == "M1":
                d["in_interval"] = d["lo"] <= meas <= d["hi"]
            rows.append(d)
        entry["predictions"] = rows
        entry["metrics"] = R.metrics(rows, form == "M1")
        entry["verdict"] = R.verdict_from(entry["metrics"])
        res[form] = entry
    res["verdict"] = res["M1"]["verdict"]
    return res


def lift_base(slug, scen, perp_all, headline):
    """S5/S6: base node value times leave-one-product-out median lift over other headline products."""
    base = BASES[scen]
    perp = perp_all[slug]
    tgt_all = R.targets_of(perp)
    bv = perp.get(base)
    res = {"scenario": scen, "base_node": {"T": base[0], "B": base[1]}, "base_value": bv}
    if bv is None:
        res.update(verdict="NOT_EVALUATED", reason="missing base node", n_train=0, n_targets=len(tgt_all))
        return res
    rows, skipped = [], []
    for k in tgt_all:
        ratios = [perp_all[o][k] / perp_all[o][base] for o in headline
                  if o != slug and base in perp_all[o] and k in perp_all[o]]
        if not ratios:
            skipped.append({"T": k[0], "B": k[1], "reason": "no other headline product has base and target node"})
            continue
        lift = float(np.median(ratios))
        pred = bv * lift
        meas = perp[k]
        rows.append({"T": k[0], "B": k[1], "measured": meas, "n_lift_products": len(ratios), "lift": lift,
                     "pred": pred, "e": pred / meas - 1.0})
    res.update(n_train=1, n_targets=len(rows), skipped_targets=skipped, predictions=rows)
    if not rows:
        res.update(verdict="NOT_EVALUATED", reason="no target nodes")
        return res
    res["metrics"] = R.metrics(rows, False)
    res["verdict"] = R.verdict_from(res["metrics"])
    return res


def s0_comparison(products, headline, s0_res):
    """v1 S0 (v1 rule) against S5/S6 over headline products evaluated in both."""
    ev = ("PASS", "FAIL", "INCONCLUSIVE")
    out = {}
    for sc in ("S5", "S6"):
        both = [s for s in headline
                if s0_res[s]["verdict"] in ev and products[s]["scenarios"][sc]["verdict"] in ev]
        a = [products[s]["scenarios"][sc]["metrics"] for s in both]
        b = [s0_res[s]["metrics"] for s in both]
        med_a = float(np.median([m["p95_abs_error"] for m in a])) if both else None
        med_b = float(np.median([m["p95_abs_error"] for m in b])) if both else None
        cnt_a = sum(1 for m in a if m["worst_overprediction"] <= R.WORST_LIMIT)
        cnt_b = sum(1 for m in b if m["worst_overprediction"] <= R.WORST_LIMIT)
        out[sc] = {"products": both, "n_products": len(both),
                   "median_p95_abs_error": med_a, "median_p95_abs_error_S0": med_b,
                   "count_worst_le_0.10": cnt_a, "count_worst_le_0.10_S0": cnt_b,
                   "improves_on_S0": bool(both and med_a < med_b and cnt_a >= cnt_b)}
    return out


def main(out):
    prot_sha = R.sha256(PROTOCOL)
    if prot_sha != PROTOCOL_SHA:
        sys.exit("protocol hash mismatch: " + prot_sha)
    sources = json.loads((R.DATA / "sources.json").read_text())["sources"]
    flags = {s["slug"]: s["not_a_tape"] for s in sources}
    slugs = sorted(flags)
    headline = [s for s in slugs if not flags[s]]
    perp_all, sf_all, counts = {}, {}, {}
    for s in slugs:
        perp_all[s], sf_all[s], counts[s] = R.read_nodes(s)
    products = {}
    s0_res = {}
    for s in slugs:
        s0_res[s] = R.lift_scenario(s, perp_all, sf_all, headline)
        products[s] = {
            "not_a_tape": flags[s], "headline": s in headline, "previously_used": s in R.PREVIOUSLY_USED,
            "row_counts": counts[s], "perpendicular_nodes": R.fmt_nodes(perp_all[s]),
            "scenarios": {
                "S3": model_scenario(perp_all[s], "S3"),
                "S4": model_scenario(perp_all[s], "S4"),
                "S5": lift_base(s, "S5", perp_all, headline),
                "S6": lift_base(s, "S6", perp_all, headline),
            },
        }
    scen = {}
    for sc in SCENARIOS:
        scen[sc] = {
            "headline": R.headline_verdict([products[s]["scenarios"][sc]["verdict"] for s in headline]),
            "previously_used": R.headline_verdict([products[s]["scenarios"][sc]["verdict"] for s in R.PREVIOUSLY_USED]),
            "ceraco": products[R.CERACO]["scenarios"][sc]["verdict"],
        }
        if sc == "S3":
            m2 = [products[s]["scenarios"][sc].get("M2", {}).get("verdict", "NOT_EVALUATED") for s in headline]
            scen[sc]["M2_secondary_headline"] = R.headline_verdict(m2)
    result = {
        "schema": "converra-cr01-results/v2",
        "protocol_sha256": prot_sha,
        "input_sha256": {
            "protocol": prot_sha,
            "sources.json": R.sha256(R.DATA / "sources.json"),
            "measurements.csv": {s: R.sha256(R.DATA / s / "measurements.csv") for s in slugs},
        },
        "runner_sha256": R.sha256(Path(__file__).resolve()),
        "v1_runner_sha256": R.sha256(R.__file__),
        "headline_products": headline,
        "scenario_summaries": scen,
        "S0_recomputed": {s: {k: v for k, v in s0_res[s].items() if k != "predictions"} | (
            {"predictions": s0_res[s]["predictions"]} if "predictions" in s0_res[s] else {}) for s in slugs},
        "S0_comparison": s0_comparison(products, headline, s0_res),
        "products": products,
    }
    Path(out).parent.mkdir(parents=True, exist_ok=True)
    Path(out).write_text(json.dumps(result, indent=1))


if __name__ == "__main__":
    main(sys.argv[1])
