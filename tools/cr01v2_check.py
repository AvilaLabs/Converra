#!/usr/bin/env python3
"""CR-01 v2 independent checker (imports neither runner).

Usage: cr01v2_check.py RESULTS.json CHECKER.json
Re-derives nodes, fits, predictions, lifts, metrics, verdicts, summaries and the S0
comparison from protocol-v2 and the prepared CSVs, using the v1 checker's
runner-independent node loader and OLS. Integer, string, bool fields exact; floats 1e-9.
"""
import json
import sys
from pathlib import Path

import numpy as np

sys.path.insert(0, str(Path(__file__).resolve().parent))
import cr01_check as K  # noqa: E402  (runner-independent)

ROOT, DATA = K.ROOT, K.DATA
PROT_PATH = ROOT / "benchmarks/cr01/protocol-v2.json"
PROT = json.loads(PROT_PATH.read_text())
TARGET_T, TARGET_B = (20, 25, 30, 35, 40), (5, 7, 8)
PREV = ["faraday-factory-japan-ybco", "shanghai-superconductor-high-field-low-temperature",
        "superpower-advanced-pinning", "theva-pro-line-advanced-pinning"]
CERACO = "ceraco-m-type-ybco-film-on-sapphire"
EVAL = ("PASS", "FAIL", "INCONCLUSIVE")


def med(v):
    s = sorted(v)
    n = len(s)
    return s[n // 2] if n % 2 else 0.5 * (s[n // 2 - 1] + s[n // 2])


def targets(perp, exclude=()):
    return sorted(k for k in perp if k[0] in TARGET_T and k[1] in TARGET_B and k not in exclude)


def model_sc(perp, sc):
    need = [(t, b) for t in {"S3": (20, 30, 40), "S4": (20, 40)}[sc] for b in {"S3": (2, 3), "S4": (3, 5)}[sc]]
    need = [(float(t), float(b)) for t, b in need]
    tg = targets(perp, need)
    r = {"n_train": sum(1 for k in need if k in perp), "n_targets": len(tg)}
    if any(k not in perp for k in need) or not tg:
        r["verdict"] = "NOT_EVALUATED"
        return r
    tr = {k: perp[k] for k in need}
    r["train"] = tr
    r["target"] = {k: perp[k] for k in tg}
    for name, m2 in (("M1", False), ("M2", True)):
        if name == "M2" and sc != "S3":
            continue
        fit = K.ols(tr, tg, m2)
        if fit is None:
            r[name] = {"verdict": "INCONCLUSIVE"}
            continue
        beta, out = fit
        es, cov, preds, los, his = [], [], [], [], []
        for k, lp, half in out:
            meas = perp[k]
            pred = float(np.exp(lp))
            es.append(pred / meas - 1)
            preds.append(pred)
            lo, hi = float(np.exp(lp - half)), float(np.exp(lp + half))
            los.append(lo)
            his.append(hi)
            cov.append(lo <= meas <= hi)
        m = K.summarize(es, None if m2 else cov)
        r[name] = {"verdict": K.verd(m), "metrics": m, "pred": preds, "e": es, "lo": los, "hi": his,
                   "Tstar": float(-1 / beta[1]), "c0": float(beta[0])}
        if m2:
            r[name].update(a0=float(-beta[2]), a1=float(-beta[3]))
        else:
            r[name]["alpha"] = float(-beta[2])
    r["verdict"] = r["M1"]["verdict"]
    return r


def lift_sc(slug, perp, sc, hl):
    base = {"S5": (70.0, 3.0), "S6": (77.5, 1.0)}[sc]
    tg = targets(perp[slug])
    if base not in perp[slug]:
        return {"verdict": "NOT_EVALUATED", "n_train": 0, "n_targets": len(tg)}
    es, preds, lifts, ns, tgk = [], [], [], [], []
    for k in tg:
        rat = [perp[o][k] / perp[o][base] for o in hl if o != slug and base in perp[o] and k in perp[o]]
        if not rat:
            continue
        lift = med(rat)
        pr = perp[slug][base] * lift
        preds.append(pr)
        lifts.append(lift)
        ns.append(len(rat))
        es.append(pr / perp[slug][k] - 1)
        tgk.append(k)
    r = {"n_train": 1, "n_targets": len(es)}
    if not es:
        r["verdict"] = "NOT_EVALUATED"
        return r
    r["metrics"] = K.summarize(es)
    r.update(pred=preds, e=es, lift=lifts, n_lift=ns, base_value=perp[slug][base], tk=tgk)
    r["verdict"] = K.verd(r["metrics"])
    return r


def main(res_path, out_path):
    res = json.loads(Path(res_path).read_text())
    c = K.Cmp()
    c.eq("protocol_sha", res["protocol_sha256"], K.sha(PROT_PATH))
    c.eq("protocol_sha_expected", res["protocol_sha256"], "73f0f67df7adb70d53c39622f90f7026a3983bc1d265729ce5a5ee3468b9b011")
    c.eq("sources_sha", res["input_sha256"]["sources.json"], K.sha(DATA / "sources.json"))
    src = json.loads((DATA / "sources.json").read_text())["sources"]
    flags = {s["slug"]: s["not_a_tape"] for s in src}
    slugs = sorted(flags)
    hl = [s for s in slugs if not flags[s]]
    c.eq("headline_list", res["headline_products"], hl)
    perp, sfv = {}, {}
    for s in slugs:
        perp[s], sfv[s] = K.load(s)
        c.eq(s + ":csv_sha", res["input_sha256"]["measurements.csv"][s], K.sha(DATA / s / "measurements.csv"))
    g3_ok = set(res["products"]) == set(slugs)
    SC = ("S3", "S4", "S5", "S6")
    allv, s0v = {}, {}
    for s in slugs:
        P = res["products"][s]
        for sc in SC:
            g3_ok = g3_ok and P["scenarios"].get(sc, {}).get("verdict") in ("PASS", "FAIL", "NOT_EVALUATED", "INCONCLUSIVE")
        rn = {(d["T"], d["B"]): d["value"] for d in P["perpendicular_nodes"]}
        c.eq(s + ":n_nodes", len(rn), len(perp[s]))
        for k, v in perp[s].items():
            c.fl("%s:node%s" % (s, k), rn.get(k, float("nan")), v)
        mine = {"S3": model_sc(perp[s], "S3"), "S4": model_sc(perp[s], "S4"),
                "S5": lift_sc(s, perp, "S5", hl), "S6": lift_sc(s, perp, "S6", hl)}
        allv[s] = mine
        s0v[s] = K.s0(s, perp, sfv, hl)
        for sc, m in mine.items():
            R = P["scenarios"][sc]
            L = "%s:%s" % (s, sc)
            c.eq(L + ":verdict", R["verdict"], m["verdict"])
            c.eq(L + ":n_train", R["n_train"], m["n_train"])
            c.eq(L + ":n_targets", R["n_targets"], m["n_targets"])
            if "train" in m:
                c.eq(L + ":train_keys", [(d["T"], d["B"]) for d in R["training_nodes"]], sorted(m["train"]))
                for d in R["training_nodes"]:
                    c.fl(L + ":train%s" % ((d["T"], d["B"]),), d["value"], m["train"].get((d["T"], d["B"]), float("nan")))
                c.eq(L + ":target_keys", [(d["T"], d["B"]) for d in R["target_nodes"]], sorted(m["target"]))
                for d in R["target_nodes"]:
                    c.fl(L + ":tgt%s" % ((d["T"], d["B"]),), d["value"], m["target"].get((d["T"], d["B"]), float("nan")))
            if sc in ("S5", "S6"):
                if "pred" in m:
                    c.fl(L + ":base", R["base_value"], m["base_value"])
                    c.eq(L + ":target_keys", [(d["T"], d["B"]) for d in R["predictions"]], m["tk"])
                    for i, row in enumerate(R["predictions"]):
                        c.eq("%s:nlift%d" % (L, i), row["n_lift_products"], m["n_lift"][i])
                        c.fl("%s:lift%d" % (L, i), row["lift"], m["lift"][i])
                        c.fl("%s:pred%d" % (L, i), row["pred"], m["pred"][i])
                        c.fl("%s:e%d" % (L, i), row["e"], m["e"][i])
                        c.fl("%s:meas%d" % (L, i), row["measured"], perp[s][(row["T"], row["B"])])
                    for key, v in m["metrics"].items():
                        (c.eq if isinstance(v, int) else c.fl)(L + ":" + key, R["metrics"][key], v)
                continue
            for name in ("M1", "M2"):
                if name not in m:
                    c.eq(L + name + ":absent", name in R, False)
                    continue
                mm, RR = m[name], R.get(name, {})
                c.eq(L + name + ":verdict", RR.get("verdict"), mm["verdict"])
                if "metrics" not in mm:
                    continue
                for key, v in mm["metrics"].items():
                    (c.eq if isinstance(v, int) else c.fl)(L + name + ":" + key, RR["metrics"][key], v)
                for i, row in enumerate(RR["predictions"]):
                    c.fl("%s%s:pred%d" % (L, name, i), row["pred"], mm["pred"][i])
                    c.fl("%s%s:e%d" % (L, name, i), row["e"], mm["e"][i])
                    c.fl("%s%s:meas%d" % (L, name, i), row["measured"], perp[s][(row["T"], row["B"])])
                    if name == "M1":
                        c.fl("%s%s:lo%d" % (L, name, i), row["lo"], mm["lo"][i])
                        c.fl("%s%s:hi%d" % (L, name, i), row["hi"], mm["hi"][i])
                        c.eq("%s%s:inint%d" % (L, name, i), row["in_interval"], mm["lo"][i] <= perp[s][(row["T"], row["B"])] <= mm["hi"][i])
                for key in ("Tstar", "c0", "alpha", "a0", "a1"):
                    if key in mm:
                        c.fl(L + name + ":" + key, RR[key], mm[key])
    for sc in SC:
        S = res["scenario_summaries"][sc]
        h = K.headline([allv[s][sc]["verdict"] for s in hl])
        p = K.headline([allv[s][sc]["verdict"] for s in PREV])
        for lab, a, b in (("headline", S["headline"], h), ("previously_used", S["previously_used"], p)):
            c.eq("%s:%s:verdict" % (sc, lab), a["verdict"], b["verdict"])
            c.eq("%s:%s:counts" % (sc, lab), a["counts"], b["counts"])
            c.eq("%s:%s:n_evaluated" % (sc, lab), a["n_evaluated"], b["n_evaluated"])
        c.eq(sc + ":ceraco", S["ceraco"], allv[CERACO][sc]["verdict"])
        if sc == "S3":
            m2 = K.headline([allv[s][sc].get("M2", {}).get("verdict", "NOT_EVALUATED") for s in hl])
            a = S["M2_secondary_headline"]
            c.eq("S3:M2:verdict", a["verdict"], m2["verdict"])
            c.eq("S3:M2:counts", a["counts"], m2["counts"])
    # S0 recomputation and comparison
    for s in slugs:
        R0 = res["S0_recomputed"][s]
        c.eq(s + ":S0:verdict", R0["verdict"], s0v[s]["verdict"])
        if "metrics" in s0v[s]:
            for key, v in s0v[s]["metrics"].items():
                (c.eq if isinstance(v, int) else c.fl)(s + ":S0:" + key, R0["metrics"][key], v)
    for sc in ("S5", "S6"):
        both = [s for s in hl if s0v[s]["verdict"] in EVAL and allv[s][sc]["verdict"] in EVAL]
        a = [allv[s][sc]["metrics"] for s in both]
        b = [s0v[s]["metrics"] for s in both]
        ma, mb = med([m["p95_abs_error"] for m in a]), med([m["p95_abs_error"] for m in b])
        ca = sum(1 for m in a if m["worst_overprediction"] <= 0.10)
        cb = sum(1 for m in b if m["worst_overprediction"] <= 0.10)
        D = res["S0_comparison"][sc]
        L = "cmp:" + sc
        c.eq(L + ":products", D["products"], both)
        c.eq(L + ":n", D["n_products"], len(both))
        c.fl(L + ":med", D["median_p95_abs_error"], ma)
        c.fl(L + ":med_S0", D["median_p95_abs_error_S0"], mb)
        c.eq(L + ":cnt", D["count_worst_le_0.10"], ca)
        c.eq(L + ":cnt_S0", D["count_worst_le_0.10_S0"], cb)
        c.eq(L + ":improves", D["improves_on_S0"], bool(ma < mb and ca >= cb))
    out = {"checker_sha256": K.sha(__file__), "runner_sha256_recorded": res["runner_sha256"],
           "results_file_sha256": K.sha(res_path), "protocol_sha256": K.sha(PROT_PATH),
           "protocol_hash_verified": K.sha(PROT_PATH) == "73f0f67df7adb70d53c39622f90f7026a3983bc1d265729ce5a5ee3468b9b011",
           "n_comparisons": c.n, "mismatches": [list(map(str, x)) for x in c.bad[:200]], "n_mismatches": len(c.bad),
           "G2": "PASS" if not c.bad else "FAIL", "G3": "PASS" if g3_ok else "FAIL"}
    Path(out_path).parent.mkdir(parents=True, exist_ok=True)
    Path(out_path).write_text(json.dumps(out, indent=1))
    print(out["G2"], out["G3"], c.n, len(c.bad))


if __name__ == "__main__":
    main(sys.argv[1], sys.argv[2])
