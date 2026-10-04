#!/usr/bin/env python3
"""CR-01 runner: implements benchmarks/cr01/protocol-v1.json literally.

Usage: cr01_run.py OUTPUT.json
"""
import csv
import hashlib
import json
import math
import sys
from pathlib import Path

import numpy as np
from scipy import stats

ROOT = Path(__file__).resolve().parent.parent
PROTOCOL = ROOT / "benchmarks/cr01/protocol-v1.json"
PROTOCOL_SHA = "f09d8331cdab54b6e4421d5fd11d186dc14e2adc1e3f817f9f3b1eb65e33915c"
DATA = ROOT / ".local/research/cr01/robinson"
GRID = [0, 0.01, 0.015, 0.02, 0.03, 0.05, 0.07, 0.1, 0.15, 0.2, 0.3, 0.5, 0.7, 1, 1.5, 2, 3, 5, 7, 8]
TARGET_T = [20, 25, 30, 35, 40]
TARGET_B = [5, 7, 8]
S1_T = (65.0, 77.5)
S1_B = (0.5, 4.0)
ANCHOR = (30.0, 3.0)
MIN_TRAIN = {"S1": 6, "S2": 7}
PREVIOUSLY_USED = [
    "faraday-factory-japan-ybco",
    "shanghai-superconductor-high-field-low-temperature",
    "superpower-advanced-pinning",
    "theva-pro-line-advanced-pinning",
]
CERACO = "ceraco-m-type-ybco-film-on-sapphire"
WORST_LIMIT = 0.10
P95_LIMIT = 0.20


def sha256(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def round_to(x, step):
    return math.floor(x / step + 0.5) * step


def snap_field(b):
    best = min(GRID, key=lambda g: abs(b - g))
    if abs(b - best) <= max(0.002, 0.02 * best):
        return float(best)
    return None


def is_perp(angle):
    return abs(angle - 180.0 * round(angle / 180.0)) < 1e-9


def read_nodes(slug):
    """Return (perp_nodes {(T,B): min Ic}, self_field value or None, row counts)."""
    perp = {}
    selfs = []
    n_rows = n_member = 0
    with open(DATA / slug / "measurements.csv", newline="") as f:
        for r in csv.DictReader(f):
            n_rows += 1
            t = float(r["nominal_temperature_k"]) if r["nominal_temperature_k"] != "" else round_to(float(r["temperature_k"]), 0.5)
            if r["nominal_field_t"] != "":
                b = float(r["nominal_field_t"])
            else:
                b = snap_field(float(r["applied_field_t"]))
                if b is None:
                    continue
            a = float(r["nominal_angle_deg"]) if r["nominal_angle_deg"] != "" else round_to(float(r["angle_from_normal_deg"]), 5.0)
            ic = float(r["ic_a_per_m"])
            n_member += 1
            if t == 77.5 and b == 0.0:
                selfs.append(ic)
            if is_perp(a):
                k = (t, b)
                perp[k] = min(perp[k], ic) if k in perp else ic
    sf = float(np.median(selfs)) if selfs else None
    return perp, sf, {"rows": n_rows, "node_member_rows": n_member, "self_field_rows": len(selfs)}


def design(nodes, form):
    cols = []
    for t, b in nodes:
        lb = math.log(b)
        row = [1.0, t, lb]
        if form == "M2":
            row.append(t * lb)
        cols.append(row)
    return np.array(cols)


def fit(train, form):
    """OLS fit of ln Ic. Returns dict; degenerate=True when not usable."""
    keys = sorted(train)
    X = design(keys, form)
    y = np.array([math.log(train[k]) for k in keys])
    p = X.shape[1]
    out = {"form": form, "n_train": len(keys), "degenerate": False}
    if np.linalg.matrix_rank(X) < p:
        out["degenerate"] = True
        out["reason"] = "rank-deficient design"
        return out
    XtXi = np.linalg.inv(X.T @ X)
    beta = XtXi @ (X.T @ y)
    res = y - X @ beta
    dof = len(keys) - p
    s2 = float(res @ res) / dof if dof > 0 else float("nan")
    ok = bool(np.all(np.isfinite(beta))) and beta[1] < 0
    out.update(beta=[float(v) for v in beta], dof=dof, s2=s2, XtXi=XtXi, form_p=p)
    if not ok:
        out["degenerate"] = True
        out["reason"] = "non-finite parameters or Tstar <= 0"
        return out
    out["c0"] = float(beta[0])
    out["Tstar"] = float(-1.0 / beta[1])
    if form == "M1":
        out["alpha"] = float(-beta[2])
    else:
        out["a0"] = float(-beta[2])
        out["a1"] = float(-beta[3])
    return out


def predict(f, target_nodes, form, interval):
    preds = {}
    X = design(target_nodes, form)
    t = float(stats.t.ppf(0.975, f["dof"])) if interval and f["dof"] > 0 else float("nan")
    beta = np.array(f["beta"])
    for k, x in zip(target_nodes, X):
        lp = float(x @ beta)
        d = {"ln_pred": lp, "pred": math.exp(lp)}
        if interval:
            se = math.sqrt(f["s2"] * (1.0 + float(x @ f["XtXi"] @ x)))
            d["ln_lo"], d["ln_hi"] = lp - t * se, lp + t * se
            d["lo"], d["hi"] = math.exp(d["ln_lo"]), math.exp(d["ln_hi"])
        preds[k] = d
    return preds


def p95(vals):
    return float(np.percentile(np.array(vals), 95))


def metrics(rows, with_cov):
    e = [r["e"] for r in rows]
    m = {"n_targets": len(rows), "worst_overprediction": max(e), "p95_abs_error": p95([abs(v) for v in e])}
    if with_cov:
        m["interval_coverage"] = sum(1 for r in rows if r["lo"] <= r["measured"] <= r["hi"]) / len(rows)
        m["n_covered"] = sum(1 for r in rows if r["lo"] <= r["measured"] <= r["hi"])
    return m


def verdict_from(m):
    return "PASS" if (m["worst_overprediction"] <= WORST_LIMIT and m["p95_abs_error"] <= P95_LIMIT) else "FAIL"


def targets_of(perp):
    return sorted(k for k in perp if k[0] in TARGET_T and k[1] in TARGET_B)


def train_nodes(perp, scen):
    tr = {k: v for k, v in perp.items() if S1_T[0] <= k[0] <= S1_T[1] and S1_B[0] <= k[1] <= S1_B[1]}
    if scen == "S2" and ANCHOR in perp:
        tr[ANCHOR] = perp[ANCHOR]
    return tr


def fmt_nodes(d):
    return [{"T": k[0], "B": k[1], "value": v} for k, v in sorted(d.items())]


def model_scenario(perp, scen):
    """S1/S2 for one product from its perpendicular node dict."""
    tgt = targets_of(perp)
    train = train_nodes(perp, scen)
    res = {"scenario": scen, "n_train": len(train), "n_targets": len(tgt),
           "training_nodes": fmt_nodes(train), "target_nodes": fmt_nodes({k: perp[k] for k in tgt})}
    if scen == "S2" and ANCHOR not in perp:
        res.update(verdict="NOT_EVALUATED", reason="missing S2 anchor (30 K, 3 T perpendicular)")
        return res
    if len(train) < MIN_TRAIN[scen]:
        res.update(verdict="NOT_EVALUATED", reason="fewer training nodes than minimum %d" % MIN_TRAIN[scen])
        return res
    if not tgt:
        res.update(verdict="NOT_EVALUATED", reason="no target nodes")
        return res
    for form in ("M1", "M2"):
        f = fit(train, form)
        entry = {k: v for k, v in f.items() if k not in ("XtXi", "form_p")}
        if f["degenerate"]:
            entry["verdict"] = "INCONCLUSIVE" if form == "M1" else "INCONCLUSIVE"
            res[form] = entry
            continue
        pr = predict(f, tgt, form, interval=(form == "M1"))
        rows = []
        for k in tgt:
            meas = perp[k]
            d = {"T": k[0], "B": k[1], "measured": meas, **pr[k]}
            d["e"] = d["pred"] / meas - 1.0
            if form == "M1":
                d["in_interval"] = d["lo"] <= meas <= d["hi"]
            rows.append(d)
        entry["predictions"] = rows
        entry["metrics"] = metrics(rows, form == "M1")
        entry["verdict"] = verdict_from(entry["metrics"])
        res[form] = entry
    res["verdict"] = res["M1"]["verdict"]
    return res


def lift_scenario(slug, perp_all, sf_all, headline):
    perp, sf = perp_all[slug], sf_all[slug]
    res = {"scenario": "S0", "self_field_value": sf}
    tgt_all = targets_of(perp)
    if sf is None:
        res.update(verdict="NOT_EVALUATED", reason="missing self-field value", n_train=0, n_targets=len(tgt_all))
        return res
    rows, skipped = [], []
    for k in tgt_all:
        ratios = []
        for o in headline:
            if o == slug or sf_all[o] is None or k not in perp_all[o]:
                continue
            ratios.append(perp_all[o][k] / sf_all[o])
        if not ratios:
            skipped.append({"T": k[0], "B": k[1], "reason": "no other headline product has this node"})
            continue
        lift = float(np.median(ratios))
        pred = sf * lift
        meas = perp[k]
        rows.append({"T": k[0], "B": k[1], "measured": meas, "n_lift_products": len(ratios), "lift": lift,
                     "pred": pred, "e": pred / meas - 1.0})
    res.update(n_train=1, n_targets=len(rows), skipped_targets=skipped, predictions=rows)
    if not rows:
        res.update(verdict="NOT_EVALUATED", reason="no target nodes")
        return res
    res["metrics"] = metrics(rows, False)
    res["verdict"] = verdict_from(res["metrics"])
    return res


def headline_verdict(verdicts):
    c = {v: 0 for v in ("PASS", "FAIL", "NOT_EVALUATED", "INCONCLUSIVE")}
    for v in verdicts:
        c[v] += 1
    evaluated = c["PASS"] + c["FAIL"] + c["INCONCLUSIVE"]
    if evaluated == 0:
        hv = "NOT_EVALUATED"
    else:
        hv = "PASS" if c["PASS"] >= 0.8 * evaluated else "FAIL"
    return {"verdict": hv, "counts": c, "n_evaluated": evaluated}


def main(out):
    prot_sha = sha256(PROTOCOL)
    if prot_sha != PROTOCOL_SHA:
        sys.exit("protocol hash mismatch: " + prot_sha)
    sources = json.loads((DATA / "sources.json").read_text())["sources"]
    flags = {s["slug"]: s["not_a_tape"] for s in sources}
    slugs = sorted(flags)
    headline = [s for s in slugs if not flags[s]]
    perp_all, sf_all, counts = {}, {}, {}
    for s in slugs:
        perp_all[s], sf_all[s], counts[s] = read_nodes(s)
    products = {}
    for s in slugs:
        products[s] = {
            "not_a_tape": flags[s], "headline": s in headline, "previously_used": s in PREVIOUSLY_USED,
            "row_counts": counts[s], "self_field_value": sf_all[s],
            "perpendicular_nodes": fmt_nodes(perp_all[s]),
            "scenarios": {
                "S0": lift_scenario(s, perp_all, sf_all, headline),
                "S1": model_scenario(perp_all[s], "S1"),
                "S2": model_scenario(perp_all[s], "S2"),
            },
        }
    scen = {}
    for sc in ("S0", "S1", "S2"):
        scen[sc] = {
            "headline": headline_verdict([products[s]["scenarios"][sc]["verdict"] for s in headline]),
            "previously_used": headline_verdict([products[s]["scenarios"][sc]["verdict"] for s in PREVIOUSLY_USED]),
            "ceraco": products[CERACO]["scenarios"][sc]["verdict"],
        }
        if sc != "S0":
            m2 = [products[s]["scenarios"][sc].get("M2", {}).get("verdict", "NOT_EVALUATED") for s in headline]
            scen[sc]["M2_secondary_headline"] = headline_verdict(m2)
    me = Path(__file__).resolve()
    result = {
        "schema": "converra-cr01-results/v1",
        "protocol_sha256": prot_sha,
        "input_sha256": {
            "protocol": prot_sha,
            "sources.json": sha256(DATA / "sources.json"),
            "tools/cr01_prepare_robinson.py": sha256(ROOT / "tools/cr01_prepare_robinson.py"),
            "measurements.csv": {s: sha256(DATA / s / "measurements.csv") for s in slugs},
        },
        "runner_sha256": sha256(me),
        "headline_products": headline,
        "scenario_summaries": scen,
        "products": products,
    }
    Path(out).parent.mkdir(parents=True, exist_ok=True)
    Path(out).write_text(json.dumps(result, indent=1))


if __name__ == "__main__":
    main(sys.argv[1])
