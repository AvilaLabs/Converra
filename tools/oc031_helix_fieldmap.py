#!/usr/bin/env python3
"""Generate the declared Cartesian field map for benchmarks/coupled/
oc-031-helix-layer.json — an independent Biot-Savart evaluation, not
the engine's kernel.

Physical model: a dense-wound helical solenoid layer — a conductor band
wound at tape-width pitch on a cylindrical former — carrying the pack's
declared reference ampere-turns smeared over K offset filaments
spanning the pack band (4 rho x 2 binormal).  The map is the winding's
*own* field only: the engine scales declared-map entries by candidate
NI, so a static environment component would mis-scale and is never
baked in (a background field the map's semantics cannot express).

The map source artifact (a CSV of the grid + field at reference NI) is
emitted next to the case and its sha256 becomes the case's
`field_map.map.source_sha256` — the same binding a customer FEA export
would carry.

Usage: python3 tools/oc031_helix_fieldmap.py
   -> benchmarks/fieldmaps/oc031-helix-map-source.csv
   -> benchmarks/coupled/oc-031-helix-layer.json  (the case itself —
      map entries are emitted from the same grid, so regeneration is
      single-source)
"""

import hashlib
import json
import os

import numpy as np

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.dirname(HERE)
OUT_DIR = os.path.join(ROOT, "benchmarks", "fieldmaps")

# ---- declared winding (must match the case's fixed_geometry) ---------
# A dense-wound helical solenoid layer: the tape's own width sets the
# rise, so turns pack shoulder-to-shoulder (a helix wound at tape-width
# pitch is a solenoid section — the classic use of the path3d slice).
R_M = 0.025            # former/cylinder radius, m
TURNS = 50.0           # helical turns along the layer
RISE_PER_TURN_M = 0.004  # axial advance per turn (one tape width)
Z0 = 0.0               # helix start z
PACK_RADIAL_M = 0.004  # rho band the pack occupies (cylinder-radial)
PACK_WIDTH_M = 0.008   # in-surface binormal band (tape width direction)
N_RHO, N_WIDTH = 4, 2  # smearing filaments across the band
# Reference total ampere-turns: at the 8x2 pack (16 conductors) each
# carries 625 A — a realistic 4 mm-tape operating current in the
# ~3 T self-field this winding produces.
REFERENCE_NI_A = 1.0e4  # total ampere-turns the map is computed at
N_HELIX_SAMPLES = 24000  # Biot-Savart line samples over the whole path

MU0 = 4e-7 * np.pi


def helix_filament(n_rho_frac, n_width_frac):
    """Offset filament positions + derivatives at uniform phi samples.

    rho offset = cylinder-radial (n_ref); width offset = in-surface
    binormal w_ref = n_ref x t — the same frame the engine declares.
    """
    phi = np.linspace(0.0, 2.0 * np.pi * TURNS, N_HELIX_SAMPLES,
                  endpoint=False)
    c = RISE_PER_TURN_M / (2.0 * np.pi)
    dr = n_rho_frac * PACK_RADIAL_M / 2.0   # ± half-band sampling
    dw = n_width_frac * PACK_WIDTH_M / 2.0

    R = R_M + dr
    x = R * np.cos(phi)
    y = R * np.sin(phi)
    z = Z0 + c * phi
    # tangent (per dphi, unnormalized) and its length
    tx, ty, tz = -R * np.sin(phi), R * np.cos(phi), c * np.ones_like(phi)
    tl = np.sqrt(tx * tx + ty * ty + tz * tz)
    tx, ty, tz = tx / tl, ty / tl, tz / tl
    # n_ref = outward cylinder radial; w_ref = n x t
    nx, ny, nz = np.cos(phi), np.sin(phi), np.zeros_like(phi)
    wx = ny * tz - nz * ty
    wy = nz * tx - nx * tz
    wz = nx * ty - ny * tx
    # offset positions and re-derived derivative
    px, py, pz = x + dw * wx, y + dw * wy, z + dw * wz
    dpx, dpy, dpz = np.gradient(px, phi), np.gradient(py, phi), \
        np.gradient(pz, phi)
    return np.stack([px, py, pz], 1), np.stack([dpx, dpy, dpz], 1), phi


def biot_savart(pos, dpos, current_a, probes):
    """B = mu0 I /4pi ∮ dl x r_hat / r^2  (dl = dpos * dphi)."""
    b = np.zeros_like(probes)
    dphi = 2.0 * np.pi * TURNS / N_HELIX_SAMPLES
    for i in range(0, len(probes), 4096):
        p = probes[i:i + 4096]                       # (m,3)
        d = p[:, None, :] - pos[None, :, :]          # (m,s,3)
        r2 = np.einsum('msd,msd->ms', d, d)
        np.maximum(r2, 1e-18, out=r2)
        inv = r2 ** -1.5
        cx = dpos[:, 1] * d[:, :, 2] - dpos[:, 2] * d[:, :, 1]
        cy = dpos[:, 2] * d[:, :, 0] - dpos[:, 0] * d[:, :, 2]
        cz = dpos[:, 0] * d[:, :, 1] - dpos[:, 1] * d[:, :, 0]
        j = slice(i, i + len(p))
        b[j, 0] += np.einsum('ms,ms->m', cx, inv) * dphi
        b[j, 1] += np.einsum('ms,ms->m', cy, inv) * dphi
        b[j, 2] += np.einsum('ms,ms->m', cz, inv) * dphi
    return MU0 * current_a / (4.0 * np.pi) * b


PARAM_FINGERPRINT = (
    f"R={R_M} TURNS={TURNS} RISE={RISE_PER_TURN_M} "
    f"PACK={PACK_RADIAL_M}x{PACK_WIDTH_M} K={N_RHO}x{N_WIDTH} "
    f"NI={REFERENCE_NI_A} N={N_HELIX_SAMPLES}"
)


def main():
    k = N_RHO * N_WIDTH
    rho_fracs = np.linspace(-0.5 + 0.5 / N_RHO, 0.5 - 0.5 / N_RHO, N_RHO)
    width_fracs = np.linspace(-0.5 + 0.5 / N_WIDTH, 0.5 - 0.5 / N_WIDTH,
                              N_WIDTH)
    xs = np.round(np.arange(-0.034, 0.034 + 1e-9, 0.004), 6)
    ys = xs.copy()
    z_end = Z0 + TURNS * RISE_PER_TURN_M
    zs = np.round(np.arange(-0.01, z_end + 0.01 + 1e-9, 0.005), 6)
    src = os.path.join(OUT_DIR, "oc031-helix-map-source.csv")

    # The Biot-Savart sweep is minutes; the CSV is regenerated only when
    # the declared parameters change (fingerprint on the comment line).
    if os.path.exists(src):
        with open(src, "rb") as f:
            cached = f.read()
        first = cached.split(b"\n", 1)[0].decode()
        if PARAM_FINGERPRINT in first:
            rows = cached.decode().splitlines()
            vals = [list(map(float, ln.split(","))) for ln in rows[2:]]
            arr = np.asarray(vals)
            probes, b = arr[:, :3], arr[:, 3:]
            sha = hashlib.sha256(cached).hexdigest()
        else:
            probes = b = sha = None
    else:
        probes = b = sha = None

    if probes is None:
        gx, gy, gz = np.meshgrid(xs, ys, zs, indexing='ij')
        probes = np.stack([gx.ravel(), gy.ravel(), gz.ravel()], 1)
        b = np.zeros_like(probes)
        for rf in rho_fracs:
            for wf in width_fracs:
                pos, dpos, _ = helix_filament(rf, wf)
                b += biot_savart(pos, dpos, REFERENCE_NI_A / k, probes)
        # map entries are tesla at REFERENCE_NI_A and are purely the
        # winding's own field — the engine scales entries by candidate
        # NI, so a static environment component must never be baked in.
        lines = [f"# {PARAM_FINGERPRINT}", "x_m,y_m,z_m,bx_t,by_t,bz_t"]
        for p, bv in zip(probes, b):
            lines.append(",".join(f"{v:.10g}" for v in (*p, *bv)))
        csv = ("\n".join(lines) + "\n").encode()
        os.makedirs(OUT_DIR, exist_ok=True)
        with open(src, "wb") as f:
            f.write(csv)
        sha = hashlib.sha256(csv).hexdigest()

    # bore anchor: evaluate the same filament model at the case's bore probe
    probe = np.array([[0.0, 0.0, z_end / 2.0]])
    ba = np.zeros((1, 3))
    for rf in rho_fracs:
        for wf in width_fracs:
            pos, dpos, _ = helix_filament(rf, wf)
            ba += biot_savart(pos, dpos, REFERENCE_NI_A / k, probe)
    anchor = float(np.linalg.norm(ba[0]))

    emit_case(xs, ys, zs, probes, b, sha, anchor, z_end)
    print(f"map: {len(probes)} nodes, sha256={sha}, anchor={anchor:.6f} T")
    print(f"case: benchmarks/coupled/oc-031-helix-layer.json")


def emit_case(xs, ys, zs, probes, b, sha, anchor, z_end):
    """Write the shipped v19 case: map entries from the same grid."""
    entries = []
    nx, ny = len(xs), len(ys)
    for i, (p, bv) in enumerate(zip(probes, b)):
        xi, yi, zi = i // (ny * len(zs)), (i // len(zs)) % ny, i % len(zs)
        entries.append({
            "x_index": xi, "y_index": yi, "z_index": zi,
            "bx_t": float(bv[0]), "by_t": float(bv[1]),
            "bz_t": float(bv[2]),
        })
    path_len = TURNS * 2.0 * np.pi * float(
        np.hypot(R_M, RISE_PER_TURN_M / (2.0 * np.pi)))
    s = [round(v * path_len, 6) for v in (0.0, 1.0 / 3, 2.0 / 3, 0.98)]
    s_ref = [round(v * path_len, 6)
             for v in (0.12, 0.25, 0.4, 0.55, 0.75, 0.88)]
    case = {
        "schema": "optcoil-coupled-search/v19",
        "id": "oc-031-helix-layer",
        "provenance": (
            "OC-031: first shipped non-planar (path3d) benchmark. A "
            "dense-wound helical solenoid layer — 50 turns at 4 mm/turn "
            "rise on a 25 mm former, 8-layer pack — screened under a "
            "*declared* Cartesian field map generated by "
            "tools/oc031_helix_fieldmap.py (independent Biot-Savart of "
            "the winding smeared over the pack band at reference NI = "
            "10000 A-turns). The map is the winding's own field only — "
            "engine-side declared maps scale linearly with candidate "
            "ampere-turns, so no static environment field is admissible "
            "in the entries. The engine's planar pack evaluator is "
            "never run on a path3d case, by schema gate. Not a "
            "built-machine claim; the geometry is a representative "
            "helical layer, prices are the standard synthetic $30/m "
            "figure."
        ),
        "requirement": {
            "bore_probe_m": [0.0, 0.0, z_end / 2.0],
            "b_target_t": anchor,
            "tolerance_fraction": 1e-6,
        },
        "fixed_geometry": {
            "path3d": {
                "segments": [{
                    "kind": "helix",
                    "axis_origin_m": [0.0, 0.0, 0.0],
                    "axis_dir": [0.0, 0.0, 1.0],
                    "radius_m": R_M,
                    "start_azimuth_deg": 0.0,
                    "turns": TURNS,
                    "rise_per_turn_m": RISE_PER_TURN_M,
                }],
                "closed": False,
            },
            "radial_pitch_m": 0.0005,
            "tape_width_m": 0.008,
            "tape_normal": "radial",
            "pack_radial_width_m": PACK_RADIAL_M,
            "pack_axial_height_m": PACK_WIDTH_M,
        },
        "field_map": {
            "map": {
                "components": "cartesian_bx_by_bz",
                "source_sha256": sha,
                "reference_ampere_turns_a": REFERENCE_NI_A,
                "x_levels_m": xs.tolist(),
                "y_levels_m": ys.tolist(),
                "z_levels_m": zs.tolist(),
                "entries": entries,
            },
            "bore_field_at_reference_t": anchor,
        },
        "choices": {
            "turns_along_normal": [4, 8],
            "tapes_along_width": [1, 2],
        },
        "operating": {
            "temperature_k": 25.0,
            "electric_field_criterion_v_per_m": 0.0001,
        },
        "material": {
            "dataset_id": "robinson-superpower-ap-v3",
            "csv_sha256":
                "889cc2cf8b91b822cbc6cceb7388e5d8afcb8a8911bb20ab175e50c7e6dc0354",
            "method": "measured-coordinate-tetrahedral-log-field-log-ic/v1",
            "angle_mapping": "period_180_field_reversal",
            "mirror_policy": "minimum_of_mirror_pair",
            "field_basis_mapping":
                "pack_field_as_applied_field_self_field_consistent",
            "field_magnitude_policy": "total_magnitude_with_transverse_angle",
            "low_field_policy": "monotone_field_lower_bound",
            "low_field_clamp_t": 1.001,
            "monotonicity_tolerance": 0.001,
        },
        "sampling": {
            "stations": [
                {"id": f"p{i}", "kind": "path", "s_m": v}
                for i, v in enumerate(s)
            ],
            "relative_turn_indices": [
                {"kind": "from_start", "offset": 1},
                {"kind": "from_end", "offset": 0},
            ],
            "width_points": 5,
        },
        "limits": {
            "max_along_current_field_fraction": 0.2,
            "max_self_field_ratio": 1.0e6,
            "interpolation_overprediction_budget": 0.1,
            "utilization_limit": 0.8,
        },
        "numerics": {
            "quadrature_orders": [2, 4],
            "field_scale_t": 1.0,
            "max_refinement_change_fraction": 0.5,
        },
        "refinement": {
            "pancake_counts": [1, 2],
            "turn_resolution": 1,
            "turn_bounds": {"min": 1, "max": 8},
            "brackets": [
                {"tapes": 1, "fail_turns": 1, "pass_turns": 8},
                {"tapes": 2, "fail_turns": 1, "pass_turns": 8},
            ],
            "monotonicity_check": True,
        },
        "pruning": None,
        "cost": {
            "price_usd_per_m": 30.0,
            "scrap_fraction": 0.1,
            "assembly_cost_per_pancake_usd": 500.0,
            "joint_cost_usd": 200.0,
        },
        "baseline": {"turns_along_normal": 8, "tapes_along_width": 1},
        "refined_plan": {
            "additional_stations": [
                {"id": f"r{i}", "kind": "path", "s_m": v}
                for i, v in enumerate(s_ref)
            ],
            "max_sampling_shortfall_fraction": 0.02,
        },
        "execution": {"max_threads": 2},
    }
    dst = os.path.join(ROOT, "benchmarks", "coupled",
                       "oc-031-helix-layer.json")
    with open(dst, "w") as f:
        json.dump(case, f, indent=1)
        f.write("\n")


if __name__ == "__main__":
    main()
