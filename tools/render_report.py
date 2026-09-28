#!/usr/bin/env python3
"""Render a coupled-search run record as a customer-facing engineering report.

The deliverable artifact: given an `optcoil-coupled-search-run/*` record,
emit the markdown report a design partner receives — the verified optimum,
the declared-baseline comparison, dollar savings, verdict honesty
(PASS/FAIL/INCONCLUSIVE/NOT_EVALUATED preserved verbatim), the per-candidate
blocker map, and the hash-bound evidence footer.

Usage:
    python3 tools/render_report.py runs/.../run-CELL.json [-o report.md]
"""

import argparse
import hashlib
import json
import os


def sha256_file(path):
    return hashlib.sha256(open(path, "rb").read()).hexdigest()


def fmt_geom(g):
    return "{} x {} x {}s".format(
        g["turns_along_normal"], g["tapes_along_width"], g["strands_parallel"]
    )


def fmt_usd(v):
    return "${:,.0f}".format(v) if v is not None else "-"


def blocker(c):
    """The physical reason an INCONCLUSIVE candidate is unresolved."""
    s = c.get("screening") or {}
    pc = s.get("point_counts") or {}
    reasons = []
    if (pc.get("unsupported") or 0) > 0:
        reasons.append("outside measured material domain")
    if (pc.get("along_current_excluded") or 0) > 0:
        reasons.append("oblique-field limit exceeded at tape edge")
    tr = s.get("max_transport_self_field_ratio")
    if not reasons and tr is not None and tr > 1.0:
        reasons.append("critical-state regime (self-field dominated)")
    return "; ".join(reasons) if reasons else "unresolved (see record)"


def main():
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("record")
    ap.add_argument("-o", "--output")
    args = ap.parse_args()

    record = json.load(open(args.record))
    case = record["case"]
    req = case["requirement"]
    cost_model = case["cost"]

    best = record["candidates"][record["best_index"]] if record.get("best_index") is not None else None
    base = (
        record["candidates"][record["baseline_index"]]
        if record.get("baseline_index") is not None
        else None
    )
    acc = record["acceptance"]
    counts = {}
    for c in record["candidates"]:
        counts[c["status"]] = counts.get(c["status"], 0) + 1

    L = []
    L.append("# OptCoil coupled-search report: {}".format(case["id"]))
    L.append("")
    L.append(
        "Requirement: >= {:.3g} T over a declared usable region "
        "(half-extents {} m) — probe at {}.".format(
            req["b_target_t"],
            req.get("good_field_region", {}).get("half_extents_m", "n/a"),
            req["bore_probe_m"],
        )
    )
    fg = case["fixed_geometry"]
    L.append(
        "Geometry family: racetrack, straight half-length {:.3f} m, "
        "bend radius {:.3f} m, tape {:.1f} mm.".format(
            fg["straight_half_length_m"], fg["bend_radius_m"], fg["tape_width_m"] * 1000
        )
    )
    L.append("")
    L.append(
        "Search: {} candidates evaluated — {}.".format(
            len(record["candidates"]),
            ", ".join("{} {}".format(v, k) for k, v in sorted(counts.items())),
        )
    )
    L.append("")

    L.append("## Result")
    L.append("")
    L.append("| | geometry (turns x tapes x strands) | installed tape | total cost | status |")
    L.append("|---|---|---|---|---|")
    if base is not None:
        L.append(
            "| Declared baseline | {} | {:,.0f} m | {} | {} |".format(
                fmt_geom(base["geometry"]),
                base["cost"]["installed_length_m"],
                fmt_usd(base["cost"]["total_usd"]),
                base["status"],
            )
        )
    if best is not None:
        L.append(
            "| **Verified optimum** | **{}** | **{:,.0f} m** | **{}** | **{}** |".format(
                fmt_geom(best["geometry"]),
                best["cost"]["installed_length_m"],
                fmt_usd(best["cost"]["total_usd"]),
                best["status"],
            )
        )
    L.append("")
    if record.get("savings_usd") is not None:
        L.append(
            "**Savings vs declared baseline: {} ({:.1f}%), "
            "acceptance agreement {}.**".format(
                fmt_usd(record["savings_usd"]),
                record.get("savings_percent", float("nan")),
                acc["agreement_status"],
            )
        )
        L.append("")

    L.append("## Acceptance")
    L.append("")
    for chk in acc.get("checks", []):
        L.append("- `{}`: **{}** — {}".format(chk["id"], chk["status"], chk.get("detail", "")))
    L.append("")

    inconclusive = [c for c in record["candidates"] if c["status"] == "INCONCLUSIVE"]
    if inconclusive:
        L.append("## Unresolved candidates — blocker map")
        L.append("")
        L.append("| geometry | blocker |")
        L.append("|---|---|")
        for c in inconclusive:
            L.append("| {} | {} |".format(fmt_geom(c["geometry"]), blocker(c)))
        L.append("")

    L.append("## Limitations")
    L.append("")
    for lim in record.get("limitations", []):
        L.append("- {}".format(lim))
    L.append(
        "- Conductor price ${:.2f}/m{}; assembly/joint costs as declared in the case.".format(
            cost_model["price_usd_per_m"],
            " (synthetic placeholder)" if cost_model["price_usd_per_m"] <= 30.0 else "",
        )
    )
    L.append("")

    L.append("## Evidence")
    L.append("")
    L.append("- record sha256: `sha256:{}`".format(sha256_file(args.record)))
    L.append("- case sha256: `{}`".format(record["case_sha256"]))
    L.append("- search model `{}`, checker `{}`".format(
        record["coupled_search_model_id"], record["coupled_search_checker_id"]))
    L.append("- dataset `{}`, csv `{}`".format(
        record["dataset_id"], record["dataset_csv_sha256"]))
    L.append("- record schema `{}`, engine `{}`".format(
        record["schema"], record.get("optcoil_version", "?")))
    L.append("")

    text = "\n".join(L)
    if args.output:
        with open(args.output, "w") as f:
            f.write(text)
        print("wrote", args.output)
    else:
        print(text)


if __name__ == "__main__":
    main()
