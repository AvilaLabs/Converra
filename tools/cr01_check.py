#!/usr/bin/env python3
"""CR-01 independent checker (does not import the runner).

Usage: cr01_check.py RESULTS.json CHECKER.json
Re-derives nodes, fits (numpy lstsq), lift factors, metrics and verdicts from the
protocol and prepared CSVs; compares with the runner JSON. Integer, string and
bool fields must match exactly; floats to relative 1e-9.
"""
import csv
import hashlib
import json
import sys
from decimal import Decimal
from pathlib import Path

import numpy as np
from scipy.special import stdtrit

ROOT = Path(__file__).resolve().parent.parent
PROT = json.loads((ROOT / "benchmarks/cr01/protocol-v1.json").read_text())
DATA = ROOT / ".local/research/cr01/robinson"
RTOL = 1e-9


def sha(p):
    return hashlib.sha256(Path(p).read_bytes()).hexdigest()


def nearest(x, step):
    # exact decimal arithmetic, ties away from +inf (half up)
    q = Decimal(repr(x)) / Decimal(repr(step))
    return float((q + Decimal("0.5")).to_integral_value(rounding="ROUND_FLOOR") * Decimal(repr(step)))


GRID = PROT["node_definition"]["field"]
GRID = [0, 0.01, 0.015, 0.02, 0.03, 0.05, 0.07, 0.1, 0.15, 0.2, 0.3, 0.5, 0.7, 1, 1.5, 2, 3, 5, 7, 8]  # as written in protocol text


def load(slug):
    nodes_perp, selfv = {}, []
    with open(DATA / slug / "measurements.csv") as fh:
        for row in csv.DictReader(fh):
            nt, nf, na = row["nominal_temperature_k"], row["nominal_field_t"], row["nominal_angle_deg"]
            T = float(nt) if nt else nearest(float(row["temperature_k"]), 0.5)
            if nf:
                B = float(nf)
            else:
                raw = float(row["applied_field_t"])
                cand = sorted(GRID, key=lambda g: (abs(g - raw), g))[0]
                if not abs(raw - cand) <= max(0.002, 0.02 * cand):
                    continue
                B = float(cand)
            A = float(na) if na else nearest(float(row["angle_from_normal_deg"]), 5.0)
            ic = float(row["ic_a_per_m"])
            if T == 77.5 and B == 0.0:
                selfv.append(ic)
            if round(A) == A and int(A) % 180 == 0:
                if (T, B) not in nodes_perp or ic < nodes_perp[(T, B)]:
                    nodes_perp[(T, B)] = ic
    s = sorted(selfv)
    n = len(s)
    sf = None if n == 0 else (s[n // 2] if n % 2 else 0.5 * (s[n // 2 - 1] + s[n // 2]))
    return nodes_perp, sf


def pct95(vals):
    v = sorted(vals)
    pos = 0.95 * (len(v) - 1)
    lo = int(pos)
    hi = min(lo + 1, len(v) - 1)
    return v[lo] + (v[hi] - v[lo]) * (pos - lo)


def ols(train, targets, m2):
    ks = sorted(train)
    def row(k):
        T, B = k
        r = [1.0, T, np.log(B)]
        if m2:
            r.append(T * np.log(B))
        return r
    A = np.array([row(k) for k in ks])
    y = np.log(np.array([train[k] for k in ks]))
    p = A.shape[1]
    beta, _, rank, _ = np.linalg.lstsq(A, y, rcond=None)
    if rank < p or not np.all(np.isfinite(beta)) or not beta[1] < 0:
        return None
    dof = len(ks) - p
    r = y - A @ beta
    s2 = float(r @ r) / dof
    cov = s2 * np.linalg.pinv(A.T @ A)
    tq = float(stdtrit(dof, 0.975))
    out = []
    for k in targets:
        x = np.array(row(k))
        lp = float(x @ beta)
        half = tq * float(np.sqrt(s2 + x @ cov @ x))
        out.append((k, lp, half))
    return beta, out


def summarize(es, cov=None):
    m = {"n_targets": len(es), "worst_overprediction": max(es), "p95_abs_error": pct95([abs(e) for e in es])}
    if cov is not None:
        m["n_covered"] = sum(cov)
        m["interval_coverage"] = sum(cov) / len(cov)
    return m


def verd(m):
    ok = m["worst_overprediction"] <= 0.10 and m["p95_abs_error"] <= 0.20
    return "PASS" if ok else "FAIL"


def headline(vs):
    c = {k: vs.count(k) for k in ("PASS", "FAIL", "NOT_EVALUATED", "INCONCLUSIVE")}
    ev = len(vs) - c["NOT_EVALUATED"]
    if ev == 0:
        return {"verdict": "NOT_EVALUATED", "counts": c, "n_evaluated": ev}
    return {"verdict": "PASS" if c["PASS"] * 5 >= 4 * ev else "FAIL", "counts": c, "n_evaluated": ev}


def model_sc(perp, sc):
    tg = sorted(k for k in perp if k[0] in (20, 25, 30, 35, 40) and k[1] in (5, 7, 8))
    tr = {k: v for k, v in perp.items() if 65 <= k[0] <= 77.5 and 0.5 <= k[1] <= 4}
    if sc == "S2" and (30.0, 3.0) in perp:
        tr[(30.0, 3.0)] = perp[(30.0, 3.0)]
    r = {"n_train": len(tr), "n_targets": len(tg)}
    if (sc == "S2" and (30.0, 3.0) not in perp) or len(tr) < (6 if sc == "S1" else 7) or not tg:
        r["verdict"] = "NOT_EVALUATED"
        return r
    for name, m2 in (("M1", False), ("M2", True)):
        fit = ols(tr, tg, m2)
        if fit is None:
            r[name] = {"verdict": "INCONCLUSIVE"}
            continue
        beta, out = fit
        es, cov, preds = [], [], []
        for k, lp, half in out:
            meas = perp[k]
            pred = float(np.exp(lp))
            es.append(pred / meas - 1)
            preds.append(pred)
            cov.append(float(np.exp(lp - half)) <= meas <= float(np.exp(lp + half)))
        m = summarize(es, cov if not m2 else None)
        r[name] = {"verdict": verd(m), "metrics": m, "pred": preds, "e": es,
                   "Tstar": float(-1 / beta[1]), "c0": float(beta[0])}
    r["verdict"] = r["M1"]["verdict"]
    return r


def s0(slug, perp, sfv, hl):
    tg = sorted(k for k in perp[slug] if k[0] in (20, 25, 30, 35, 40) and k[1] in (5, 7, 8))
    r = {"n_train": 0 if sfv[slug] is None else 1}
    if sfv[slug] is None:
        r.update(verdict="NOT_EVALUATED", n_targets=len(tg))
        return r
    es, preds = [], []
    for k in tg:
        rat = sorted(perp[o][k] / sfv[o] for o in hl if o != slug and sfv[o] is not None and k in perp[o])
        if not rat:
            continue
        n = len(rat)
        lift = rat[n // 2] if n % 2 else 0.5 * (rat[n // 2 - 1] + rat[n // 2])
        pr = sfv[slug] * lift
        preds.append(pr)
        es.append(pr / perp[slug][k] - 1)
    r["n_targets"] = len(es)
    if not es:
        r["verdict"] = "NOT_EVALUATED"
        return r
    r["metrics"] = summarize(es)
    r["pred"], r["e"] = preds, es
    r["verdict"] = verd(r["metrics"])
    return r


# ---------- comparison ----------
class Cmp:
    def __init__(self):
        self.n = 0
        self.bad = []

    def eq(self, label, a, b):
        self.n += 1
        if a != b:
            self.bad.append((label, a, b))

    def fl(self, label, a, b):
        self.n += 1
        if not (abs(a - b) <= RTOL * max(abs(a), abs(b)) or a == b):
            self.bad.append((label, a, b))


def main(res_path, out_path):
    res = json.loads(Path(res_path).read_text())
    c = Cmp()
    c.eq("protocol_sha", res["protocol_sha256"], sha(ROOT / "benchmarks/cr01/protocol-v1.json"))
    src = json.loads((DATA / "sources.json").read_text())["sources"]
    flags = {s["slug"]: s["not_a_tape"] for s in src}
    slugs = sorted(flags)
    hl = [s for s in slugs if not flags[s]]
    prev = ["faraday-factory-japan-ybco", "shanghai-superconductor-high-field-low-temperature",
            "superpower-advanced-pinning", "theva-pro-line-advanced-pinning"]
    perp, sfv = {}, {}
    for s in slugs:
        perp[s], sfv[s] = load(s)
        c.eq(s + ":csv_sha", res["input_sha256"]["measurements.csv"][s], sha(DATA / s / "measurements.csv"))
    # G3 coverage accounting
    g3_ok = set(res["products"]) == set(slugs)
    allv = {}
    for s in slugs:
        P = res["products"][s]
        for sc in ("S0", "S1", "S2"):
            g3_ok = g3_ok and P["scenarios"].get(sc, {}).get("verdict") in ("PASS", "FAIL", "NOT_EVALUATED", "INCONCLUSIVE")
        # nodes
        rn = {(d["T"], d["B"]): d["value"] for d in P["perpendicular_nodes"]}
        c.eq(s + ":n_nodes", len(rn), len(perp[s]))
        for k, v in perp[s].items():
            c.fl("%s:node%s" % (s, k), rn.get(k, float("nan")), v)
        if sfv[s] is None:
            c.eq(s + ":sf", P["self_field_value"], None)
        else:
            c.fl(s + ":sf", P["self_field_value"], sfv[s])
        mine = {"S0": s0(s, perp, sfv, hl), "S1": model_sc(perp[s], "S1"), "S2": model_sc(perp[s], "S2")}
        allv[s] = mine
        for sc, m in mine.items():
            R = P["scenarios"][sc]
            L = "%s:%s" % (s, sc)
            c.eq(L + ":verdict", R["verdict"], m["verdict"])
            c.eq(L + ":n_train", R["n_train"], m["n_train"])
            c.eq(L + ":n_targets", R["n_targets"], m["n_targets"])
            models = [("S0", m)] if sc == "S0" else [(n, m[n]) for n in ("M1", "M2") if n in m]
            for name, mm in models:
                RR = R if sc == "S0" else R.get(name)
                if sc != "S0":
                    c.eq(L + name + ":verdict", RR["verdict"], mm["verdict"])
                if "metrics" not in mm:
                    continue
                for key, v in mm["metrics"].items():
                    if isinstance(v, int):
                        c.eq(L + name + ":" + key, RR["metrics"][key], v)
                    else:
                        c.fl(L + name + ":" + key, RR["metrics"][key], v)
                for i, row in enumerate(RR["predictions"]):
                    c.fl("%s%s:pred%d" % (L, name, i), row["pred"], mm["pred"][i])
                    c.fl("%s%s:e%d" % (L, name, i), row["e"], mm["e"][i])
                if "Tstar" in mm:
                    c.fl(L + name + ":Tstar", RR["Tstar"], mm["Tstar"])
                    c.fl(L + name + ":c0", RR["c0"], mm["c0"])
    # headlines
    for sc in ("S0", "S1", "S2"):
        S = res["scenario_summaries"][sc]
        h = headline([allv[s][sc]["verdict"] for s in hl])
        p = headline([allv[s][sc]["verdict"] for s in prev])
        for lab, a, b in (("headline", S["headline"], h), ("previously_used", S["previously_used"], p)):
            c.eq("%s:%s:verdict" % (sc, lab), a["verdict"], b["verdict"])
            c.eq("%s:%s:counts" % (sc, lab), a["counts"], b["counts"])
        c.eq(sc + ":ceraco", S["ceraco"], allv["ceraco-m-type-ybco-film-on-sapphire"][sc]["verdict"])
    g2 = not c.bad
    out = {"checker_sha256": sha(__file__), "runner_sha256_recorded": res["runner_sha256"],
           "results_file_sha256": sha(res_path), "protocol_sha256": sha(ROOT / "benchmarks/cr01/protocol-v1.json"),
           "n_comparisons": c.n, "mismatches": [list(map(str, b)) for b in c.bad[:200]], "n_mismatches": len(c.bad),
           "G2": "PASS" if g2 else "FAIL", "G3": "PASS" if g3_ok else "FAIL"}
    Path(out_path).write_text(json.dumps(out, indent=1))
    print(out["G2"], out["G3"], c.n, len(c.bad))


if __name__ == "__main__":
    main(sys.argv[1], sys.argv[2])
