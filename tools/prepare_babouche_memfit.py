#!/usr/bin/env python3
"""Emit declared model-fit material datasets from Babouche et al. 2026.

Source: R. Babouche et al., "Practical scaling parameters for critical
current anisotropy in commercial REBCO coated conductors for high field
applications", Supercond. Sci. Technol. (2026), doi:10.1088/1361-6668/ae940e
(CC BY 4.0).

The paper publishes Maximum Entropy Model fit parameters at measured (B,T)
conditions (tables 2 and 3) and prescribes linear interpolation of the MEM
parameters in magnetic field and temperature to generate Ic(B,theta,T)
profiles at intermediate conditions (their fig. 11 workflow). This tool:

1. Takes the *text-published* parameter values only (no figure digitization).
2. Interpolates each MEM parameter linearly inside the quadrilateral spanned
   by the four published (B,T) corners, following the paper's stated recipe:
   for node (T,B), the parameter value is linear in T along each quad edge
   and linear in B across the quad at fixed T.  Nodes are emitted ONLY at
   (T,B) inside/on the quad; no extrapolation is performed.
3. Evaluates the published MEM equations (eqs. 1-2: Lorentzian + Gaussian
   components; theta_L=0 deg, theta_G=90 deg per the paper's stated
   interpolation convention) on a 2 deg grid of `angle_from_normal_deg`
   (Babouche theta_B = 90 - theta_ours + periodicity handled by the MEM's
   180-deg period: theta_B = (theta_ours + 90) mod 180).
4. Emits a dataset bundle per tape identical in layout to
   data/materials/robinson-*:  material.json, measurements.csv,
   preparation-audit.json, README.md.

Every row is a model-fit evaluation, not a measurement: data_class is
"published_model_fit".  No verdict produced with these datasets is
measured-data-verified anywhere; they quantify which candidates *would*
pass if the published MEM parametrization held at those conditions.

Angular component sets follow the paper: below 40 K the model is two
Lorentzians plus one Gaussian (G2/G3 are defined only at >=40 K in the
source), so grid nodes are emitted only for T <= 35 K.

Two dataset ids are emitted per tape: `-v1` on the original grid
(23 in-quad nodes, unchanged byte-for-byte) and `-v2` on a grid
extended toward the quad's 19 T corner (same parameters, denser T
levels near 20 K).  v2 is a new dataset identity — v1 stays embedded
so cases pinned to it are unaffected.
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
OUT = os.path.join(ROOT, "data/materials")

# ---- published MEM parameters (paper tables 2 and 3) ----------------------
# Each corner: (B[T], T[K]) -> {component: (I0*, Gamma)}.  I0* is normalized
# to the tape's Ic(77 K, s.f.) (table 1).  theta_L=0, theta_G=90 per the
# paper's interpolation convention; published fitted centers were 0/90+-3.
#   A = (14 T, 5 K)   table 3 (paper's own interpolated node)
#   B = (7.5 T, 20 K) table 3
#   C = (19 T, 20 K)  table 2 (fitted at measured condition)
#   D = (10 T, 55 K)  table 2 (fitted; extra G2/G3 components unused: the
#                     grid stays <= 35 K in the 2L+G1 regime)
_SUPERPOWER_CORNERS = {
    "A": {"B": 14.0, "T": 5.0,
          "L1": (17.02, 0.34), "L2": (37.75, 0.73), "G1": (1.74, 0.21)},
    "B": {"B": 7.5, "T": 20.0,
          "L1": (6.90, 0.18), "L2": (37.19, 0.79), "G1": (1.61, 0.18)},
    "C": {"B": 19.0, "T": 20.0,
          "L1": (2.778, 0.068), "L2": (16.537, 0.420), "G1": (1.889, 0.250)},
    "D": {"B": 10.0, "T": 55.0,
          "L1": (0.094, 0.090), "L2": (2.278, 0.930), "G1": (0.500, 0.400)},
}
_SST_CORNERS = {
    "A": {"B": 14.0, "T": 5.0,
          "L1": (3.50, 0.13), "L2": (6.78, 0.84), "G1": (0.21, 0.29)},
    "B": {"B": 7.5, "T": 20.0,
          "L1": (1.40, 0.08), "L2": (6.13, 0.84), "G1": (0.11, 0.25)},
    "C": {"B": 19.0, "T": 20.0,
          "L1": (0.826, 0.043), "L2": (3.325, 0.750), "G1": (0.055, 0.250)},
    "D": {"B": 10.0, "T": 55.0,
          "L1": (0.182, 0.017), "L2": (0.894, 0.844), "G1": (0.005, 0.100)},
}

# Quad vertex order in (B,T) space: A bottom, B left, C right, D top.
V = {"A": (14.0, 5.0), "B": (7.5, 20.0), "C": (19.0, 20.0), "D": (10.0, 55.0)}

# v1 grid: the original emitted set (23 in-quad nodes).
T_LEVELS_V1 = [10.0, 15.0, 20.0, 25.0, 30.0, 35.0]
B_LEVELS_V1 = [8.0, 10.0, 12.0, 14.0, 16.0]
# v2 grid: extended toward the quad's high-field corner (19 T @ 20 K)
# and densified in T near 20 K where the quad is widest.  Still no
# extrapolation: nodes outside the published quad are never emitted.
T_LEVELS_V2 = [10.0, 15.0, 17.5, 20.0, 22.5, 25.0, 30.0, 35.0]
B_LEVELS_V2 = [8.0, 10.0, 12.0, 14.0, 16.0, 17.0, 18.0, 19.0]

TAPES = {
    "babouche-superpower-m31477-memfit-v1": {
        "material": "SuperPower M3-1477-8 0508 REBCO coated conductor",
        "sample_id": "M3-1477-8 0508",
        "ic77sf_a": 59.0,
        "tape_width_m": 0.004,
        "corners": _SUPERPOWER_CORNERS,
        "t_levels": T_LEVELS_V1,
        "b_levels": B_LEVELS_V1,
    },
    "babouche-superpower-m31477-memfit-v2": {
        "material": "SuperPower M3-1477-8 0508 REBCO coated conductor",
        "sample_id": "M3-1477-8 0508",
        "ic77sf_a": 59.0,
        "tape_width_m": 0.004,
        "corners": _SUPERPOWER_CORNERS,
        "t_levels": T_LEVELS_V2,
        "b_levels": B_LEVELS_V2,
        "extra_limitations": [
            "v2 extends the emitted grid toward the quad's high-field corner: 17-19 T rows exist only where the published parameter quad covers them (all of them at T in [15, 25] K; 19 T exists only at T=20 K exactly). High-field coverage is therefore narrow in temperature — queries between emitted (T,B) nodes are interpolations of the model fit, never extrapolations.",
        ],
    },
    "babouche-sst-yp506-memfit-v1": {
        "material": "Shanghai Superconductor Technology YP-506 REBCO coated conductor",
        "sample_id": "YP-506",
        "ic77sf_a": 171.0,
        "tape_width_m": 0.004,
        "corners": _SST_CORNERS,
        "t_levels": T_LEVELS_V1,
        "b_levels": B_LEVELS_V1,
    },
    "babouche-sst-yp506-memfit-v2": {
        "material": "Shanghai Superconductor Technology YP-506 REBCO coated conductor",
        "sample_id": "YP-506",
        "ic77sf_a": 171.0,
        "tape_width_m": 0.004,
        "corners": _SST_CORNERS,
        "t_levels": T_LEVELS_V2,
        "b_levels": B_LEVELS_V2,
        "extra_limitations": [
            "v2 extends the emitted grid toward the quad's high-field corner: 17-19 T rows exist only where the published parameter quad covers them (all of them at T in [15, 25] K; 19 T exists only at T=20 K exactly). High-field coverage is therefore narrow in temperature — queries between emitted (T,B) nodes are interpolations of the model fit, never extrapolations.",
        ],
    },
}

ANGLE_STEP_DEG = 2.0
N_VALUE = 20.0  # declared placeholder; the source reports Ic only


def sha256_bytes(b: bytes) -> str:
    return hashlib.sha256(b).hexdigest()


def edge_point(v1, v2, t):
    """point on quad edge v1->v2 at absolute temperature t (both span it)."""
    (b1, t1), (b2, t2) = V[v1], V[v2]
    f = (t - t1) / (t2 - t1)
    b = b1 + f * (b2 - b1)
    return b, f


def quad_bounds(t):
    """(B_left, params_left_key, frac_l, B_right, key_r, frac_r) at temp t."""
    if t <= 20.0:
        bl, fl = edge_point("A", "B", t)
        br, fr = edge_point("A", "C", t)
        return bl, ("A", "B", fl), br, ("A", "C", fr)
    bl, fl = edge_point("B", "D", t)
    br, fr = edge_point("C", "D", t)
    return bl, ("B", "D", fl), br, ("C", "D", fr)


def interp_params(corners, t, b):
    """bilinear parameter interpolation on the quad (paper's recipe)."""
    bl, (lv1, lv2, fl), br, (rv1, rv2, fr) = quad_bounds(t)
    if b < bl - 1e-9 or b > br + 1e-9:
        return None
    fb = 0.0 if br - bl < 1e-12 else (b - bl) / (br - bl)
    out = {}
    for comp in ("L1", "L2", "G1"):
        for i in (0, 1):  # I0*, Gamma
            pl = (corners[lv1][comp][i]
                  + fl * (corners[lv2][comp][i] - corners[lv1][comp][i]))
            pr = (corners[rv1][comp][i]
                  + fr * (corners[rv2][comp][i] - corners[rv1][comp][i]))
            out[(comp, i)] = pl + fb * (pr - pl)
    return out


def mem_ic_star(theta_b_deg, p):
    """MEM Ic/Ic(77K,sf): two Lorentzians + one Gaussian (paper eqs. 1-2)."""
    th = math.radians(theta_b_deg)
    total = 0.0
    for comp, center_deg in (("L1", 0.0), ("L2", 0.0), ("G1", 90.0)):
        i0, g = p[(comp, 0)], p[(comp, 1)]
        d = math.radians(theta_b_deg - center_deg)
        if comp.startswith("L"):
            c2, s2 = math.cos(d) ** 2, math.sin(d) ** 2
            total += (i0 / (math.pi * g)) / (c2 + (1.0 / g) ** 2 * s2)
        else:
            t2 = math.tan(d) ** 2
            if t2 > 700.0 * 2.0 * g * g:  # exp underflow -> contribution ~0
                continue
            sec2 = 1.0 + t2
            total += (i0 / (math.sqrt(2 * math.pi) * g)) * sec2 * math.exp(
                -t2 / (2.0 * g * g))
    return total


def build(tape_id, spec):
    corners = spec["corners"]
    rows = []
    emitted = []  # (T,B) nodes actually inside the quad
    src = 800_000
    for t in spec["t_levels"]:
        for b in spec["b_levels"]:
            p = interp_params(corners, t, b)
            if p is None:
                continue
            emitted.append((t, b))
            n_ang = int(round(180.0 / ANGLE_STEP_DEG)) + 1
            for k in range(n_ang):
                th_ours = k * ANGLE_STEP_DEG           # angle from tape normal
                th_b = (th_ours + 90.0) % 180.0        # Babouche frame
                ic = mem_ic_star(th_b, p) * spec["ic77sf_a"]
                rows.append({
                    "source_row": src,
                    "nominal_temperature_k": t,
                    "nominal_field_t": b,
                    "nominal_angle_deg": th_ours,
                    "temperature_k": t,
                    "applied_field_t": b,
                    "angle_from_normal_deg": th_ours,
                    "ic_a_per_m": ic / spec["tape_width_m"],
                    "bridge_ic_a": ic,
                    "n_value": N_VALUE,
                })
                src += 1
    return rows, emitted


def write_dataset(tape_id, spec):
    dst = os.path.join(OUT, tape_id)
    os.makedirs(dst, exist_ok=True)
    rows, emitted = build(tape_id, spec)

    buf = io.StringIO()
    w = csv.DictWriter(buf, fieldnames=[
        "source_row", "nominal_temperature_k", "nominal_field_t",
        "nominal_angle_deg", "temperature_k", "applied_field_t",
        "angle_from_normal_deg", "ic_a_per_m", "bridge_ic_a", "n_value",
    ], lineterminator="\n")
    w.writeheader()
    for r in rows:
        w.writerow(r)
    csv_bytes = buf.getvalue().encode()
    csv_sha = sha256_bytes(csv_bytes)

    # source artifact: the published parameter table this build consumes
    params_src = json.dumps(
        {"source_doi": "10.1088/1361-6668/ae940e",
         "tables": {"2": "fitted params at (10T,55K) and (19T,20K)",
                    "3": "interpolated params at (14T,5K),(7.5T,20K),(12T,20K FFJ)"},
         "corners": spec["corners"]},
        sort_keys=True, indent=1).encode()
    params_sha = sha256_bytes(params_src)
    with open(os.path.join(dst, "mem-params.json"), "wb") as f:
        f.write(params_src)

    with open(__file__, "rb") as f:
        prep_sha = sha256_bytes(f.read())

    t_emit = sorted({t for t, _ in emitted})
    b_emit = sorted({b for _, b in emitted})
    meta = {
        "schema": "optcoil-measured-material/v2",
        "id": tape_id,
        "data_class": "published_model_fit",
        "material": spec["material"],
        "sample_id": spec["sample_id"],
        "source_doi": "10.1088/1361-6668/ae940e",
        "source_url": "https://iopscience.iop.org/article/10.1088/1361-6668/ae940e",
        "authors": [
            "Romain Babouche", "Christian Barth", "Jovica Badel",
            "Davide Uglietti", "Carmine Senatore",
        ],
        "license": "CC-BY-4.0",
        "license_url": "https://creativecommons.org/licenses/by/4.0/",
        "measurement_dates": "tapes received 2023; article accepted 2026",
        "source_xlsx_sha256": params_sha,
        "csv_sha256": csv_sha,
        "preparation_source_sha256": prep_sha,
        "source_description_sha256": sha256_bytes(b"placeholder"),
        "electric_field_criterion_v_per_m": 0.0001,
        "voltage_tap_spacing_m": 0.005,
        "measured_bridge_width_m": spec["tape_width_m"],
        "original_tape_width_m": spec["tape_width_m"],
        "field_basis": "applied_field_including_sample_self_field_response",
        "angle_convention": "oriented_from_tape_normal_in_maximum_lorentz_geometry",
        "coordinate_policy": "Nominal grid nodes; measured coordinate columns repeat the nominal node. Every node is a MEM-model evaluation: published parameters bilinearly interpolated inside the published (B,T) parameter quad, then the published equations evaluated on a 2 deg angular grid.",
        "normalization": "Published I0* parameters normalized to the tape's Ic(77 K, s.f.) (paper table 1); absolute Ic per 4 mm tape; ic_a_per_m = Ic/0.004.",
        "measurement_uncertainty_fraction": None,
        "strain_state": "not_applicable_model_fit",
        "selection": {
            "nominal_temperature_k": t_emit,
            "nominal_field_t": b_emit,
            "nominal_angle_range_deg": [0.0, 180.0],
        },
        "max_cell_spans": {
            "temperature_k": 12.0,
            "field_ratio": 1.5,
            "angle_deg": 5.0,
        },
        "point_count": len(rows),
        "limitations": [
            "MODEL-FIT DATASET: every row is an evaluation of the published maximum-entropy-model parametrization (Babouche et al. 2026, doi:10.1088/1361-6668/ae940e, CC BY 4.0), not a measurement. No verdict produced with this dataset is measured-data-verified.",
            "MEM parameters are published only at four (B,T) corners {(14,5),(7.5,20),(19,20),(10,55)}; grid nodes between them use the paper's prescribed linear-in-(B,T) parameter interpolation. Interpolated interior nodes are model-generated twice over (fit + parameter interpolation).",
            "Domain is the published parameter quad in (B,T): T<=35 K keeps the two-Lorentzian+Gaussian component set the paper defines below 40 K; the >=40 K G2/G3 components are not emitted.",
            "Angular positions fixed at theta_L=0 deg, theta_G=90 deg per the paper's interpolation convention (fitted centers were 0/90+-3 deg).",
            "Published-fit residual vs the paper's measured anchors is nonzero: e.g. SuperPower 19 T/20 K c-axis Ic fit 312 A vs measured 268 A (+16%); SST 152 A vs 136 A (+12%). Relative angular anisotropy is the meaningful content, not absolute capacity.",
            "n_value is a declared placeholder (20.0); the source reports critical current only. voltage_tap_spacing_m is nominal; the UNIGE full-tape tap geometry is not published.",
            "Full-width 4 mm tape data; not a per-lot manufacturer guarantee.",
            *spec.get("extra_limitations", []),
        ],
    }

    with open(os.path.join(dst, "measurements.csv"), "wb") as f:
        f.write(csv_bytes)
    with open(os.path.join(dst, "material.json"), "w") as f:
        json.dump(meta, f, indent=1)

    audit = {
        "generator": os.path.relpath(__file__, ROOT),
        "dataset_id": tape_id,
        "method": "published MEM parameters (tables 2-3) -> bilinear-in-(B,T) parameter interpolation inside the published quad -> MEM eqs. 1-2 on 2 deg angular grid",
        "emitted_nodes": emitted,
        "skipped_nodes_outside_quad": [
            [t, b] for t in spec["t_levels"] for b in spec["b_levels"]
            if (t, b) not in emitted],
        "point_count": len(rows),
        "csv_sha256": csv_sha,
        "params_sha256": params_sha,
        "preparation_source_sha256": prep_sha,
    }
    with open(os.path.join(dst, "preparation-audit.json"), "w") as f:
        json.dump(audit, f, indent=1)

    readme = (
        f"# {tape_id}\n\n"
        "Declared model-fit dataset generated from the published MEM\n"
        "parameters of Babouche et al. 2026 (doi:10.1088/1361-6668/ae940e,\n"
        "CC BY 4.0).  Every row is a model evaluation, not a measurement;\n"
        "see material.json `limitations` and preparation-audit.json.\n"
    )
    with open(os.path.join(dst, "README.md"), "w") as f:
        f.write(readme)

    # bind the README hash now that it exists
    meta["source_description_sha256"] = sha256_bytes(readme.encode())
    with open(os.path.join(dst, "material.json"), "w") as f:
        json.dump(meta, f, indent=1)

    print(f"{tape_id}: {len(rows)} rows, nodes={len(emitted)}, "
          f"csv_sha256={csv_sha}")


def main():
    for tape_id, spec in TAPES.items():
        write_dataset(tape_id, spec)


if __name__ == "__main__":
    main()
