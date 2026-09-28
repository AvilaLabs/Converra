#!/usr/bin/env python3
"""Independent OC-002 reference: analytically integrate the axial coordinate.

This implementation never imports or executes OptCoil's Rust solver. The Rust
kernel integrates a 3D volume. Here the z integral is exact and only a 2D planar
integral remains, evaluated with NumPy Gauss-Legendre quadrature and triangular
Duffy maps at the projected observation point. All fields are SI point values.

Dependencies: Python 3, NumPy. No network access or commercial solver required.
Run from any directory; the output is exclusively created, never overwritten.
"""

import argparse
import hashlib
import json
import math
import platform
import time
from pathlib import Path

import numpy as np
from numpy.polynomial.legendre import leggauss


def cells(geometry):
    """Fixed 16 subdivisions per component, independent of Rust's sizing rule."""
    a = geometry["straight_half_length_m"]
    r = geometry["bend_radius_m"]
    width = geometry["radial_width_m"]
    radial = (r - width / 2, r + width / 2)
    for kind, begin, end in (
        ("top", -a, a),
        ("bottom", -a, a),
        ("right", -math.pi / 2, math.pi / 2),
        ("left", math.pi / 2, 3 * math.pi / 2),
    ):
        if begin == end:
            continue
        boundaries = np.linspace(begin, end, 17)
        for lo, hi in zip(boundaries[:-1], boundaries[1:]):
            yield kind, np.array([lo, radial[0]]), np.array([hi, radial[1]])


def planar_source(kind, q, a):
    s, rho = q
    if kind in ("top", "bottom"):
        sign = 1 if kind == "top" else -1
        return s, sign * rho, np.full_like(s, -sign), np.zeros_like(s), 1.0
    center = a if kind == "right" else -a
    co, si = np.cos(s), np.sin(s)
    return center + rho * co, rho * si, -si, co, rho


def projected_parameters(kind, point, a, lo, hi):
    x, y, _ = point
    if kind in ("top", "bottom"):
        projected = [x, y if kind == "top" else -y]
    else:
        dx = x - (a if kind == "right" else -a)
        mid = (lo[0] + hi[0]) / 2
        theta = math.atan2(y, dx)
        theta = mid + math.atan2(math.sin(theta - mid), math.cos(theta - mid))
        projected = [theta, math.hypot(dx, y)]
    return np.clip(projected, lo, hi)


def field(geometry, point, order):
    nodes, weights = leggauss(order)
    nodes, weights = (nodes + 1) / 2, weights / 2
    u, v = np.meshgrid(nodes, nodes, indexing="ij")
    weights2 = weights[:, None] * weights[None, :]
    a = geometry["straight_half_length_m"]
    height = geometry["axial_height_m"]
    zlo, zhi = point[2] - height / 2, point[2] + height / 2
    terms = []
    for kind, lo, hi in cells(geometry):
        center = projected_parameters(kind, point, a, lo, hi)
        for e0 in (lo[0] - center[0], hi[0] - center[0]):
            for e1 in (lo[1] - center[1], hi[1] - center[1]):
                if e0 == 0 or e1 == 0:
                    continue
                # Two triangles tile each rectangle. The u Jacobian cancels
                # the remaining integrable planar 1/r singularity.
                for dominant in (0, 1):
                    q = np.array([center[0] + e0 * u, center[1] + e1 * u * v])
                    if dominant == 1:
                        q = np.array([center[0] + e0 * u * v, center[1] + e1 * u])
                    sx, sy, tx, ty, jac = planar_source(kind, q, a)
                    dx, dy = point[0] - sx, point[1] - sy
                    d2 = dx * dx + dy * dy
                    if np.any(d2 == 0):
                        raise ArithmeticError("reference quadrature sampled the planar singularity")
                    low, high = np.sqrt(d2 + zlo * zlo), np.sqrt(d2 + zhi * zhi)
                    # Exact antiderivatives in the source's axial coordinate.
                    transverse = 1 / low - 1 / high
                    axial = (zhi / high - zlo / low) / d2
                    vector = np.array([ty * transverse, -tx * transverse, (tx * dy - ty * dx) * axial])
                    factor = weights2 * abs(e0 * e1) * u * jac
                    terms.append(np.sum(vector * factor, axis=(1, 2)))
    integral = np.array([math.fsum(t[k] for t in terms) for k in range(3)])
    return integral * (1e-7 * geometry["ampere_turns_a"] / (geometry["radial_width_m"] * height))


def circular_pack_axis(geometry, z):
    """Closed-form finite rectangular annulus on axis, used to audit reference."""
    ri = geometry["bend_radius_m"] - geometry["radial_width_m"] / 2
    ro = geometry["bend_radius_m"] + geometry["radial_width_m"] / 2
    half_h = geometry["axial_height_m"] / 2

    def primitive(t):
        if t == 0:
            return 0.0
        return t * (math.asinh(ro / abs(t)) - math.asinh(ri / abs(t)))

    density = geometry["ampere_turns_a"] / (geometry["radial_width_m"] * 2 * half_h)
    return 2 * math.pi * 1e-7 * density * (primitive(z + half_h) - primitive(z - half_h))


def main():
    root = Path(__file__).resolve().parents[1]
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--case", type=Path, default=root / "benchmarks/synthetic/oc-002.json")
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    raw = args.case.read_bytes()
    case = json.loads(raw)
    if case["schema"] != "optcoil-magnetostatics/v1" or case["geometry"]["current_model"] != "uniform_winding_pack":
        raise ValueError("unsupported reference assumptions")
    started = time.perf_counter()
    orders = [24, 48, 72]
    rows = []
    for probe in case["probes"]:
        values = [field(case["geometry"], probe["position_m"], n) for n in orders]
        changes = [float(np.linalg.norm(b - a)) for a, b in zip(values[:-1], values[1:])]
        rows.append({"id": probe["id"], "position_m": probe["position_m"], "fields_t": [v.tolist() for v in values], "refinement_changes_t": changes})
        print(f"{probe['id']}: B={values[-1]} T; last refinement={changes[-1]:.3e} T", flush=True)
    # Analytic finite annulus audit is a separate geometry with the same pack.
    circular = dict(case["geometry"], straight_half_length_m=0.0)
    audits = []
    for z in (0.0, 0.013, 0.2):
        expected = circular_pack_axis(circular, z)
        computed = field(circular, (0.0, 0.0, z), orders[-1])
        error = float(np.linalg.norm(computed - [0, 0, expected]))
        if error > 1e-10:
            raise ArithmeticError(f"reference failed annulus check: {error} T")
        audits.append({"z_m": z, "expected_bz_t": expected, "computed_field_t": computed.tolist(), "vector_error_t": error})
    max_change = max(row["refinement_changes_t"][-1] for row in rows)
    if max_change / case["acceptance"]["field_scale_t"] > case["acceptance"]["max_reference_refinement_fraction"]:
        raise ArithmeticError("reference did not satisfy its frozen refinement threshold")
    record = {
        "schema": "optcoil-field-reference/v1",
        "case_sha256": hashlib.sha256(raw).hexdigest(),
        "method": "analytical-z-integral-planar-duffy-gauss/v1",
        "source_sha256": hashlib.sha256(Path(__file__).read_bytes()).hexdigest(),
        "source_path": "tools/reference_oc002.py",
        "python_version": platform.python_version(),
        "numpy_version": np.__version__,
        "platform": platform.platform(),
        "orders": orders,
        "component_subdivisions": 16,
        "mu0_h_per_m": 4 * math.pi * 1e-7,
        "probes": rows,
        "analytic_audits": audits,
        "elapsed_seconds": time.perf_counter() - started,
        "limitations": ["Prescribed uniform volume current and vacuum permeability; no nonlinear HTS material response.", "Independent numerical implementation, not experimental or commercial FEM validation.", "Refinement differences are convergence observations, not rigorous error bounds."]
    }
    with args.output.open("x") as output:
        json.dump(record, output, indent=2, allow_nan=False)
        output.write("\n")
    print(f"Wrote {args.output}; max reference refinement {max_change:.3e} T")


if __name__ == "__main__":
    main()
