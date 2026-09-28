#!/usr/bin/env python3
"""OC-016: model-extended variant of robinson-superpower-ap-v3-lowfield.

The measured dataset ends at 8 T applied field; OC-012's coverage-blocked
candidates need Ic(T, B, angle) up to ~17 T. No measured data exists there
yet (see docs/RESEARCH_ANCHORS.md), so this tool emits a *labeled model
extension*: every measured row is copied byte-identically, and additional
nodes on an extended nominal field grid carry Ic from an anchored power-law
continuation fitted per (temperature, angle) curve.

Model, per curve:  Ic(B) = Ic_measured(B_anchor) * (B / B_anchor)^(-alpha)
  B_anchor = the curve's measured ~8 T node; alpha = max(
      least-squares log-log slope over the 3-8 T tail,
      local 7->8 T slope,
      0.1  (decay floor; REBCO Ic cannot be flat/increasing in B)).
Anchoring to the measured node keeps the extension continuous with the
data by construction. A global conservative margin, derived from held-out
high-field edge checks, is then applied to every modeled node.

Held-out edge checks (the honest validation available without >8 T data):
  fitA: slope from {1.5,2,3,5} T -> predict measured 7 T and 8 T
  fitB: slope from {3,5,7} T     -> predict measured 8 T
The largest over-prediction across all 215 curves sets the margin floor;
a +5% pad is added on top. Modeled nodes therefore sit at/below what the
same procedure would have predicted at the edge of the measured domain.

This is a model, not measurement: data_class is
"measured_with_model_extension", modeled nodes carry source_row >= 900000,
and no verdict produced with this dataset may be reported as
measured-data-verified. It exists to quantify which candidates *would*
pass if high-field data confirmed the continuation -- a sensitivity map,
not a certification basis.
"""

import csv
import hashlib
import io
import json
import math
import os
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
SRC = os.path.join(ROOT, "data/materials/robinson-superpower-ap-v3-lowfield")
DST = os.path.join(ROOT, "data/materials/robinson-superpower-ap-v3-modelext")

EXTENDED_FIELDS_T = [10.0, 12.5, 16.0, 20.0]
ANCHOR_FIELD_T = 8.0
TAIL_FIT_FIELDS_T = [3.0, 5.0, 7.0, 8.0]
ALPHA_FLOOR = 0.1
MARGIN_PAD = 0.05
MODELED_SOURCE_ROW_BASE = 900_000
BRIDGE_WIDTH_M = 0.001


def sha256_bytes(data: bytes) -> str:
    return hashlib.sha256(data).hexdigest()


def loglog_slope(points):
    """Least-squares d(ln Ic)/d(ln B) over [(B, Ic)] -> slope (negative)."""
    xs = [math.log(b) for b, _ in points]
    ys = [math.log(ic) for _, ic in points]
    n = len(points)
    sx, sy = sum(xs), sum(ys)
    sxx = sum(x * x for x in xs)
    sxy = sum(x * y for x, y in zip(xs, ys))
    return (n * sxy - sx * sy) / (n * sxx - sx * sx)


def main():
    with open(os.path.join(SRC, "measurements.csv"), "rb") as f:
        raw = f.read()
    src_meta = json.load(open(os.path.join(SRC, "material.json")))

    text = raw.decode("utf-8")
    lines = text.splitlines()
    header, body = lines[0], lines[1:]
    rows = list(csv.DictReader(io.StringIO(text)))
    assert len(rows) == len(body)

    curves = {}
    for r in rows:
        key = (float(r["nominal_temperature_k"]), float(r["nominal_angle_deg"]))
        curves.setdefault(key, []).append(r)
    for pts in curves.values():
        pts.sort(key=lambda r: float(r["nominal_field_t"]))

    # Held-out edge checks: how well does this extrapolation family predict
    # the highest measured fields it was not fitted on?
    edge_errors = []  # (curve, predicted_field, signed_relative_error)
    for key, pts in curves.items():
        by_nf = {float(r["nominal_field_t"]): r for r in pts}
        for train_fields, pred_fields in (
            ([1.5, 2.0, 3.0, 5.0], [7.0, 8.0]),
            ([3.0, 5.0, 7.0], [8.0]),
        ):
            train = [
                (float(by_nf[f]["applied_field_t"]), float(by_nf[f]["ic_a_per_m"]))
                for f in train_fields
                if f in by_nf
            ]
            if len(train) < 3:
                continue
            s = loglog_slope(train)
            alpha = max(-s, ALPHA_FLOOR)
            b0, ic0 = train[-1]
            for pf in pred_fields:
                if pf not in by_nf:
                    continue
                r = by_nf[pf]
                b, ic_meas = float(r["applied_field_t"]), float(r["ic_a_per_m"])
                ic_pred = ic0 * (b / b0) ** (-alpha)
                edge_errors.append((key, pf, ic_pred / ic_meas - 1.0))

    pos = sorted(e for _, _, e in edge_errors if e > 0)
    neg = [e for _, _, e in edge_errors if e <= 0]
    max_over = pos[-1] if pos else 0.0
    p95_over = pos[int(0.95 * len(pos)) - 1] if pos else 0.0
    max_under = min(neg) if neg else 0.0
    margin = max_over + MARGIN_PAD
    print(
        f"edge checks: {len(edge_errors)} predictions | over-pred max {max_over:+.3f} "
        f"p95 {p95_over:+.3f} | under-pred max {max_under:+.3f} | margin {margin:.3f}"
    )

    audit_curves = []
    modeled_rows = []
    idx = 0
    for key, pts in sorted(curves.items()):
        by_nf = {float(r["nominal_field_t"]): r for r in pts}
        anchor = by_nf[ANCHOR_FIELD_T]
        tail = [
            (float(by_nf[f]["applied_field_t"]), float(by_nf[f]["ic_a_per_m"]))
            for f in TAIL_FIT_FIELDS_T
            if f in by_nf
        ]
        s_fit = loglog_slope(tail)
        a_fit = max(-s_fit, ALPHA_FLOOR)
        b7, b8 = by_nf[7.0], by_nf[8.0]
        a_local = max(
            -math.log(float(b8["ic_a_per_m"]) / float(b7["ic_a_per_m"]))
            / math.log(float(b8["applied_field_t"]) / float(b7["applied_field_t"])),
            ALPHA_FLOOR,
        )
        alpha = max(a_fit, a_local)
        b_anchor = float(anchor["applied_field_t"])
        ic_anchor = float(anchor["ic_a_per_m"])
        audit_curves.append(
            {
                "nominal_temperature_k": key[0],
                "nominal_angle_deg": key[1],
                "alpha_tail_fit": round(a_fit, 4),
                "alpha_local_7_8": round(a_local, 4),
                "alpha_used": round(alpha, 4),
                "anchor_applied_field_t": b_anchor,
                "anchor_ic_a_per_m": ic_anchor,
            }
        )
        for level in EXTENDED_FIELDS_T:
            ic = ic_anchor * (level / b_anchor) ** (-alpha) * (1.0 - margin)
            idx += 1
            modeled_rows.append(
                [
                    str(MODELED_SOURCE_ROW_BASE + idx),
                    f"{key[0]:g}",
                    f"{level:g}",
                    f"{key[1]:g}",
                    anchor["temperature_k"],
                    f"{level:g}",
                    anchor["angle_from_normal_deg"],
                    f"{ic:.3f}",
                    f"{ic * BRIDGE_WIDTH_M:.4f}",
                    anchor["n_value"],
                ]
            )

    # Measured rows stay byte-identical: copy their source lines verbatim.
    out_lines = [header] + body + [",".join(r) for r in modeled_rows]
    out_csv = ("\n".join(out_lines) + "\n").encode("utf-8")
    csv_hash = sha256_bytes(out_csv)
    self_hash = sha256_bytes(open(os.path.abspath(__file__), "rb").read())

    sel = dict(src_meta["selection"])
    sel["nominal_field_t"] = sel["nominal_field_t"] + EXTENDED_FIELDS_T
    meta = dict(src_meta)
    meta.update(
        {
            "schema": "optcoil-measured-material/v2",
            "id": "robinson-superpower-ap-v3-modelext",
            "data_class": "measured_with_model_extension",
            "csv_sha256": csv_hash,
            "preparation_source_sha256": self_hash,
            "selection": sel,
            "point_count": len(rows) + len(modeled_rows),
            "limitations": src_meta["limitations"]
            + [
                "MODEL EXTENSION: nodes at nominal field 10/12.5/16/20 T are not "
                "measurements. They are anchored power-law continuations "
                f"(alpha = max(3-8 T tail fit, 7-8 T local slope, {ALPHA_FLOOR}); "
                f"uniform conservative margin {margin:.3f}) emitted by "
                "tools/prepare_oc016.py; modeled rows carry source_row >= "
                f"{MODELED_SOURCE_ROW_BASE}.",
                "No verdict produced with this dataset is measured-data-verified "
                "above 8 T; it is a sensitivity map for the coverage-blocked "
                "candidate population, pending real high-field data "
                "(docs/RESEARCH_ANCHORS.md).",
                f"Edge validation: the same continuation fitted on data ending "
                f"at 5-7 T over-predicts the measured 7/8 T nodes by at most "
                f"{max_over * 100:.1f}% across all curves; the applied margin "
                f"exceeds that bound.",
            ],
        }
    )

    audit = {
        "tool": "tools/prepare_oc016.py",
        "tool_sha256": self_hash,
        "source_dataset": "robinson-superpower-ap-v3-lowfield",
        "source_csv_sha256": src_meta["csv_sha256"],
        # Measured rows are copied verbatim: the measured section of the
        # output is byte-identical to the source file minus its header.
        "measured_rows_byte_identical": sha256_bytes(("\n".join(body) + "\n").encode())
        == sha256_bytes(text[len(header) + 1 :].encode()),
        "model": "anchored_power_law_per_T_angle_curve",
        "anchor_field_t": ANCHOR_FIELD_T,
        "tail_fit_fields_t": TAIL_FIT_FIELDS_T,
        "alpha_floor": ALPHA_FLOOR,
        "extended_fields_t": EXTENDED_FIELDS_T,
        "edge_checks": {
            "predictions": len(edge_errors),
            "max_over_prediction": max_over,
            "p95_over_prediction": p95_over,
            "max_under_prediction": max_under,
            "margin_applied": margin,
            "margin_pad": MARGIN_PAD,
        },
        "curves": audit_curves,
        "modeled_point_count": len(modeled_rows),
        "output_csv_sha256": csv_hash,
    }

    os.makedirs(DST, exist_ok=True)
    with open(os.path.join(DST, "measurements.csv"), "wb") as f:
        f.write(out_csv)
    with open(os.path.join(DST, "material.json"), "w") as f:
        json.dump(meta, f, indent=2)
        f.write("\n")
    with open(os.path.join(DST, "preparation-audit.json"), "w") as f:
        json.dump(audit, f, indent=2)
        f.write("\n")
    print(
        f"wrote {DST}: {len(rows)} measured + {len(modeled_rows)} modeled "
        f"= {meta['point_count']} rows; csv sha {csv_hash[:16]}..."
    )


if __name__ == "__main__":
    sys.exit(main())
