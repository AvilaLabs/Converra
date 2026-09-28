#!/usr/bin/env python3
"""Independent OC-004 reference: coupled racetrack field + measured bridge law.

This tool never imports or executes any part of the Rust workspace. It reuses
only four pure functions from tools/reference_oc002.py (`cells`,
`planar_source`, `projected_parameters`, `field` -- the independent
analytical-z / planar-Duffy NumPy field kernel; see that file's own docstring)
and otherwise re-implements, from the OC-004 contract's textual description
alone, everything OC-004 adds on top of OC-002:

  * station geometry (top straight + right bend), tape positions, and the
    5-point Gauss-Lobatto width quadrature (contract A4/A7);
  * the angle mapping: fold to [0,180) under field-reversal symmetry, then
    take the minimum over the {theta, 180-theta} mirror pair (A5);
  * a from-scratch six-tetrahedra, measured-coordinate, log-field/log-Ic
    interpolator over data/materials/robinson-superpower-ap-v3/measurements.csv,
    built by reading the contract's description of
    crates/optcoil-physics/src/critical_current.rs (nominal-grid connectivity,
    complete cells only, per-cell span limits from material.json, six fixed
    corner permutations, containment tolerance 1e-10 in scaled coordinates) --
    the tiling *rule* is matched, but no code or search order is copied from
    that file, and this module never imports it;
  * the low-field lower-bound clamp sequence and K_used (A8);
  * a near-face-graded variant of OC-002's field kernel, `field_graded`
    (TASK K2), used for every field evaluation in this file instead of the
    plain `reference_oc002.field`.

Unit field. The OC-002 kernel's own normalization already divides by the pack
cross-section and multiplies by `geometry["ampere_turns_a"]`, and its
linearity in ampere_turns_a is a tested Rust property (racetrack.rs). Calling
it once per point per order with ampere_turns_a = 1.0 therefore yields the
field per ampere-turn; the actual field at the evaluated current is that
unit field scaled by (total_turns * evaluated_current_a), with
total_turns = turns_along_normal * tapes_along_width (A1).

Near-face graded quadrature (TASK K2). OC-004 samples points at or inside
the winding-pack cross-section (tape centers), unlike OC-002's exterior
probes. reference_oc002.field() tiles each rectangular source cell into two
Duffy triangles per corner, parametrized by a dominant coordinate u (full
extent, first-power Jacobian, already resolves the near-corner 1/r-type
singularity) and a free coordinate v in [0, 1] (the non-dominant axis,
scaled by u). When a query point's projection lands close to one edge of a
cell whose other in-plane extent is much larger -- which happens routinely
in OC-004, e.g. a tape center ~0.15 mm from a radial pack face inside a cell
whose along-path extent is centimeters -- that triangle's short-to-long
extent ratio r = |short extent| / |long extent| becomes small (~1e-2 to
1e-3), and the plain order-point Gauss-Legendre rule in v under-resolves the
resulting narrow near-face structure (see the build log's numerical
diagnosis: 6.7x-62x over the declared refinement gates at orders [10,14] /
[24,48]). `field_graded` below reproduces `reference_oc002.field` exactly,
except that whenever a Duffy triangle's own r < 0.25 the v coordinate is
resolved with a composite Gauss-Legendre rule geometrically graded toward
the near corner -- panels [0, r], [r, 4r], [4r, 16r], ..., [., 1] on [0, 1],
each an independent order-point Gauss-Legendre panel -- while the dominant
coordinate u keeps the same, unchanged, single-panel order-point rule.
`reference_oc002.field` itself is untouched and still imported unchanged
(and is still what tools/reference_oc002.py's own OC-002 reference uses);
only this file's OC-004 reference now calls `field_graded` instead.

Metric-consistent grading ratio (OC-008 contract Stage K, kernel v3 -- a
later, separate task from the near-face grading feature above, whose own
"(TASK K2)" labels predate it). The short/long ratio above was formed from
the raw Duffy-triangle extents e0, e1 in each cell's own coordinates. For
"top"/"bottom" (straight) cells both are metres, but for "right"/"left"
(arc) cells e0 is azimuth in radians while e1 is a radial metre extent, so
comparing them directly made the ratio differ from the physical aspect
ratio by the local radius. `field_graded` now scales e0 by the projected
center's own radius (`center[1]`) before forming the ratio on arc cells
only; straight cells are unchanged, and the actual (e0, e1) quadrature
mapping is untouched -- only how the grading ratio is measured changed.
This is the same fix applied to `transverse_grading_ratio` in
crates/optcoil-physics/src/racetrack.rs; see docs/OC002.md and
docs/OC004.md for before/after numbers.

Known limitations of THIS reference tool (not of OC-004 generally):
  * A8 defines the estimate/lower_bound/unsupported basis test per mirror
    angle, but the file format in contract Sec. 3 carries one `basis` and one
    `query_field_t` per width point, not per angle. This tool aggregates the
    two per-angle bases with the contract's own point-basis rule (estimate
    iff both angles are estimates; unsupported if either is unsupported;
    otherwise lower_bound) and reports `query_field_t` as the field value
    consistent with that aggregate basis (the raw magnitude for `estimate`
    and `unsupported`, the clamp value for `lower_bound`). The per-angle K
    values (k_folded/k_mirror) are still exact, independently obtained
    results; only this one summary field is a documented simplification.
  * No monotonicity audit is performed here -- that is the embedded-dataset
    check the Rust runner performs, and the separate sub-1T check performed
    by tools/audit_oc004_material_assumptions.py against the archived
    workbook. This tool applies the declared clamp mechanically per A8.
  * Only the `radial` tape_normal path is exercised by the frozen case, but
    both `radial` and `axial` geometries are implemented per the contract's
    A4 formulas, since the contract requires `axial` to be unit-tested
    elsewhere in the project.
  * Pure NumPy/Python, double precision; no independent cross-check of
    reference_oc002.py's own field kernel beyond what that file already
    performs (its own analytic annulus audit).

Dependencies: Python 3, NumPy. No network access. The output file is
exclusively created ("x" mode), never overwritten.
"""

import argparse
import bisect
import csv
import hashlib
import io
import json
import math
import platform
import sys
import time
from pathlib import Path

import numpy as np
from numpy.polynomial.legendre import leggauss

sys.path.insert(0, str(Path(__file__).resolve().parent))
from reference_oc002 import cells, planar_source, projected_parameters, field  # noqa: F401

ROOT = Path(__file__).resolve().parents[1]
MU0_H_PER_M = 4 * math.pi * 1e-7

# Panels grow by this ratio away from the near corner (contract TASK K2:
# "[0, r], [r, 4r], ..., [., 1]"). Grading only engages below this
# short/long extent ratio; at or above it the plain rule is unchanged.
GRADING_RATIO = 4.0
GRADING_THRESHOLD = 0.25

# Gauss-Lobatto, 5 points on [-1, 1] (both edges included), weights halved so
# they sum to 1 rather than 2 (contract A7).
GL5_NODES = (-1.0, -math.sqrt(3.0 / 7.0), 0.0, math.sqrt(3.0 / 7.0), 1.0)
GL5_WEIGHTS = tuple(w / 2.0 for w in (1.0 / 10.0, 49.0 / 90.0, 32.0 / 45.0, 49.0 / 90.0, 1.0 / 10.0))

METHOD = (
    "analytical-z-integral-planar-duffy-gauss-graded"
    "+independent-measured-coordinate-tetrahedral-log-ic/v1"
)
# OC-014: emitted when the case declares limits.self_field_correction
# (coupled-conductor schema v2). The query magnitude becomes
# |B_applied| + mu0*K_tape/2, matching evaluate_candidate_point's
# uniform-transport bound; the Rust checker requires this method id
# exactly for corrected cases.
METHOD_V2 = (
    "analytical-z-integral-planar-duffy-gauss-graded"
    "+independent-measured-coordinate-tetrahedral-log-ic"
    "+uniform-transport-self-field/v2"
)
# OC-017 (coupled-conductor schema v3): emitted when the case declares
# limits.along_current_model = "transverse_bound" — optionally stacked on
# the v2 self-field correction. Points past the declared along-current
# fraction limit but at or below this ceiling are still queried at full
# magnitude and the transverse-plane angle, labeled "along_current_bounded"
# (a bounded estimate, not a clean measured-plane one); past the ceiling
# they remain "along_current_excluded".
METHOD_V3 = (
    "analytical-z-integral-planar-duffy-gauss-graded"
    "+independent-measured-coordinate-tetrahedral-log-ic"
    "+uniform-transport-self-field/v2"
    "+along-current-transverse-bound/v1"
)
# The along-current bound without the self-field correction stacked.
METHOD_ALONG_CURRENT = (
    "analytical-z-integral-planar-duffy-gauss-graded"
    "+independent-measured-coordinate-tetrahedral-log-ic"
    "+along-current-transverse-bound/v1"
)
# OC-014 Phase 2 (coupled-conductor schema v4): emitted when the case
# declares limits.self_field_correction = "critical_state_strip". The query
# magnitude is |B_applied| + B_edge where B_edge is the strip's
# critical-state edge self-field bound and d is the declared current-layer
# thickness. Valid at any transport ratio — the uniform bound's dominance
# gate does not apply. Method /v3 (Phase 2b): B_edge is the largest fixed
# point of B_edge = (mu0/2pi)*k_bound(|B_app|+B_edge)*ln(w/d), where
# k_bound is the suffix-max bounding table over every measured angle level
# — the edge zone's critical sheet current at the field it actually sees,
# whatever direction its field takes. Field-consistent and still a bound:
# strictly tighter than the low-field-floor value (method /v2).
METHOD_CRITICAL_STATE = (
    "analytical-z-integral-planar-duffy-gauss-graded"
    "+independent-measured-coordinate-tetrahedral-log-ic"
    "+critical-state-strip-edge-self-field/v3"
)
METHOD_CRITICAL_STATE_ALONG_CURRENT = (
    "analytical-z-integral-planar-duffy-gauss-graded"
    "+independent-measured-coordinate-tetrahedral-log-ic"
    "+critical-state-strip-edge-self-field/v3"
    "+along-current-transverse-bound/v1"
)
ALONG_CURRENT_BOUND_CEILING = 0.5


def sha256_bytes(data):
    return hashlib.sha256(data).hexdigest()


# ---------------------------------------------------------------------------
# OC-014 Phase 2b: field-consistent critical-state edge-field bound.
# Mirrors crates/optcoil-search/src/coupled.rs's CriticalStateFieldTable —
# the bounding table and bisection must produce bit-identical values.
# ---------------------------------------------------------------------------


def build_critical_state_table(interpolator, temperature_k, low_field_clamp_t):
    """(evaluated_field, suffix_max_k) entries: at each nominal field level
    (clamped up to the case's low-field floor), the largest Ic resolvable
    over the whole measured angle axis, then suffix-maxed so the table is a
    non-increasing bounding function of field. None when nothing resolves —
    the caller marks every point unboundable (unsupported), never an
    uncorrected-field pass."""
    entries = []
    for level in interpolator.axes[1]:
        evaluated = max(level, low_field_clamp_t)
        if entries and entries[-1][0] == evaluated:
            continue
        k_max = None
        for angle in interpolator.axes[2]:
            r = interpolator.evaluate((temperature_k, evaluated, angle))
            if r is not None:
                k_max = r[0] if k_max is None else max(k_max, r[0])
        if k_max is not None:
            entries.append((evaluated, k_max))
    for i in range(len(entries) - 2, -1, -1):
        entries[i] = (entries[i][0], max(entries[i][1], entries[i + 1][1]))
    return entries or None


def cs_k_bound(table, field_t):
    """Suffix-max bound at field_t: the entry at the greatest evaluated
    level not above field_t (below the first -> first entry, the clamp
    regime; above the last -> last entry — support is decided by the
    material query this bound only raises)."""
    keys = [b for b, _ in table]
    i = bisect.bisect_right(keys, field_t) - 1
    return table[max(0, min(i, len(table) - 1))][1]


def cs_consistent_edge_field(table, applied_magnitude, tape_width_m, layer_thickness_m, low_field_clamp_t):
    """Largest fixed point of B_edge = (mu0/2pi)*k_bound(|B_app|+B_edge)*
    ln(w/d), by bisection on g(B) = B - f(B) over [0, B_floor] — the
    returned hi endpoint satisfies hi >= f(hi): still a bound."""
    coeff = (MU0_H_PER_M / (2.0 * math.pi)) * math.log(tape_width_m / layer_thickness_m)
    lo, hi = 0.0, coeff * cs_k_bound(table, low_field_clamp_t)
    for _ in range(40):
        mid = 0.5 * (lo + hi)
        if coeff * cs_k_bound(table, max(applied_magnitude + mid, low_field_clamp_t)) > mid:
            lo = mid
        else:
            hi = mid
    return hi


def dot(a, b):
    return a[0] * b[0] + a[1] * b[1] + a[2] * b[2]


def cross(a, b):
    return (
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    )


# ---------------------------------------------------------------------------
# Near-face graded field kernel (contract TASK K2; module docstring above)
# ---------------------------------------------------------------------------


def graded_unit_rule(order, r):
    """Gauss-Legendre nodes/weights on [0, 1]. Below GRADING_THRESHOLD, a
    composite rule geometrically graded toward 0 with panels [0, r],
    [r, 4r], [4r, 16r], ..., [., 1] (each an independent order-point
    Gauss-Legendre panel); otherwise the plain single-panel order-point
    rule, identical to reference_oc002.field()'s own (nodes+1)/2, weights/2
    transform."""
    base_nodes, base_weights = leggauss(order)
    base_nodes = (base_nodes + 1.0) / 2.0
    base_weights = base_weights / 2.0
    if not (0.0 < r < GRADING_THRESHOLD):
        return base_nodes, base_weights
    bounds = [0.0, r]
    while bounds[-1] < 1.0:
        bounds.append(min(bounds[-1] * GRADING_RATIO, 1.0))
    nodes_parts, weights_parts = [], []
    for lo, hi in zip(bounds[:-1], bounds[1:]):
        width = hi - lo
        nodes_parts.append(lo + width * base_nodes)
        weights_parts.append(width * base_weights)
    return np.concatenate(nodes_parts), np.concatenate(weights_parts)


def field_graded(geometry, point, order):
    """Reproduces reference_oc002.field() exactly, except that each Duffy
    triangle's transverse (non-dominant) coordinate v is resolved with
    graded_unit_rule() using r = |short extent| / |long extent| of that
    triangle's (e0, e1) pair, instead of the plain order-point rule. The
    dominant coordinate u is untouched: same leggauss(order) transform,
    same single panel, for every triangle."""
    u_nodes, u_weights = leggauss(order)
    u_nodes, u_weights = (u_nodes + 1.0) / 2.0, u_weights / 2.0
    a = geometry["straight_half_length_m"]
    height = geometry["axial_height_m"]
    zlo, zhi = point[2] - height / 2, point[2] + height / 2
    terms = []
    for kind, lo, hi in cells(geometry):
        center = projected_parameters(kind, point, a, lo, hi)
        # TASK K2 (OC-008 contract Stage K, kernel v3): coordinate 0 is
        # metres for "top"/"bottom" (straight) cells but azimuth in radians
        # for "right"/"left" (arc) cells, while coordinate 1 is always a
        # radial metre extent. Comparing e0 and e1 directly, as before,
        # makes the short/long ratio differ from the physical aspect ratio
        # by the local radius on arc cells. Scale e0 by the projected
        # center's own radius (center[1]) before forming the ratio on arc
        # cells only; straight cells are unchanged. This matches
        # racetrack.rs's transverse_grading_ratio fix exactly: only how the
        # grading ratio is measured changes, never the actual (e0, e1)
        # quadrature mapping below.
        radial_metric_m = center[1] if kind in ("right", "left") else 1.0
        for e0 in (lo[0] - center[0], hi[0] - center[0]):
            for e1 in (lo[1] - center[1], hi[1] - center[1]):
                if e0 == 0 or e1 == 0:
                    continue
                short, long_ = sorted((abs(e0) * radial_metric_m, abs(e1)))
                v_nodes, v_weights = graded_unit_rule(order, short / long_)
                u, v = np.meshgrid(u_nodes, v_nodes, indexing="ij")
                weights2 = u_weights[:, None] * v_weights[None, :]
                # Two triangles tile each rectangle, exactly as in
                # reference_oc002.field(); only the v rule above changed.
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
                    transverse = 1 / low - 1 / high
                    axial = (zhi / high - zlo / low) / d2
                    vector = np.array([ty * transverse, -tx * transverse, (tx * dy - ty * dx) * axial])
                    factor = weights2 * abs(e0 * e1) * u * jac
                    terms.append(np.sum(vector * factor, axis=(1, 2)))
    integral = np.array([math.fsum(t[k] for t in terms) for k in range(3)])
    return integral * (1e-7 * geometry["ampere_turns_a"] / (geometry["radial_width_m"] * height))


# ---------------------------------------------------------------------------
# Station geometry, tape positions, local frames (contract A4)
# ---------------------------------------------------------------------------


def straight_frame(x_s):
    return {
        "t": (-1.0, 0.0, 0.0),
        "r": (0.0, 1.0, 0.0),
        "z": (0.0, 0.0, 1.0),
        "point": lambda rho, z: (x_s, rho, z),
    }


def bend_frame(bend_center_x, azimuth_deg):
    phi = math.radians(azimuth_deg)
    c, s = math.cos(phi), math.sin(phi)
    return {
        "t": (-s, c, 0.0),
        "r": (c, s, 0.0),
        "z": (0.0, 0.0, 1.0),
        "point": lambda rho, z: (bend_center_x + rho * c, rho * s, z),
    }


def station_frame(station, straight_half_length_m):
    if station["kind"] == "straight":
        x_s = station["x_m"]
        if abs(x_s) > straight_half_length_m + 1e-9:
            raise ValueError(f"station {station['id']!r}: x_m outside straight_half_length_m")
        return straight_frame(x_s)
    if station["kind"] == "arc":
        azimuth = station["azimuth_deg"]
        if not -90.0 - 1e-9 <= azimuth <= 90.0 + 1e-9:
            raise ValueError(f"station {station['id']!r}: azimuth_deg outside [-90, 90]")
        # Right bend is centered at (+straight_half_length_m, 0, 0) (A4).
        return bend_frame(straight_half_length_m, azimuth)
    raise ValueError(f"station {station['id']!r}: unknown kind {station['kind']!r}")


def tape_position(pack, winding, frame, turn_index, tape_index, xi):
    """Position, (n_hat, w_hat, t_hat) for one width-quadrature sample."""
    tape_width = winding["tape_width_m"]
    if winding["tape_normal"] == "radial":
        pitch_n = pack["radial_width_m"] / winding["turns_along_normal"]
        rho = (pack["bend_radius_m"] - pack["radial_width_m"] / 2.0) + (turn_index - 0.5) * pitch_n
        pitch_w = pack["axial_height_m"] / winding["tapes_along_width"]
        z_center = -pack["axial_height_m"] / 2.0 + (tape_index - 0.5) * pitch_w
        z = z_center + xi * (tape_width / 2.0)
        n_hat, w_hat = frame["r"], frame["z"]
    elif winding["tape_normal"] == "axial":
        pitch_n = pack["axial_height_m"] / winding["turns_along_normal"]
        z = -pack["axial_height_m"] / 2.0 + (turn_index - 0.5) * pitch_n
        pitch_w = pack["radial_width_m"] / winding["tapes_along_width"]
        rho_center = (pack["bend_radius_m"] - pack["radial_width_m"] / 2.0) + (tape_index - 0.5) * pitch_w
        rho = rho_center + xi * (tape_width / 2.0)
        n_hat, w_hat = frame["z"], frame["r"]
    else:
        raise ValueError(f"unknown tape_normal {winding['tape_normal']!r}")
    position = frame["point"](rho, z)
    return position, n_hat, w_hat, frame["t"]


# ---------------------------------------------------------------------------
# Angle mapping (contract A5)
# ---------------------------------------------------------------------------


def fold_angle(theta_raw_deg):
    folded = ((theta_raw_deg % 180.0) + 180.0) % 180.0
    if abs(folded - 180.0) < 1e-9:
        folded = 0.0
    return folded


def mirror_angle(theta_folded_deg):
    return 180.0 if theta_folded_deg == 0.0 else 180.0 - theta_folded_deg


# ---------------------------------------------------------------------------
# Independent six-tetrahedra measured-coordinate log-field/log-Ic interpolator
# ---------------------------------------------------------------------------

PERMUTATIONS = ((0, 1, 2), (0, 2, 1), (1, 0, 2), (1, 2, 0), (2, 0, 1), (2, 1, 0))
CONTAINMENT_ROUNDOFF = 1e-10


def read_material_points(csv_bytes):
    text = csv_bytes.decode("utf-8")
    reader = csv.DictReader(io.StringIO(text))
    points = []
    for row in reader:
        points.append(
            {
                "source_row": int(row["source_row"]),
                "nominal_temperature_k": float(row["nominal_temperature_k"]),
                "nominal_field_t": float(row["nominal_field_t"]),
                "nominal_angle_deg": float(row["nominal_angle_deg"]),
                "temperature_k": float(row["temperature_k"]),
                "applied_field_t": float(row["applied_field_t"]),
                "angle_from_normal_deg": float(row["angle_from_normal_deg"]),
                "ic_a_per_m": float(row["ic_a_per_m"]),
            }
        )
    return points


def nominal_position(p):
    angle = p["nominal_angle_deg"]
    return (p["nominal_temperature_k"], p["nominal_field_t"], 0.0 if angle == 0.0 else angle)


def actual_position(p):
    return (p["temperature_k"], p["applied_field_t"], p["angle_from_normal_deg"])


class MeasuredCoordinateLogInterpolator:
    """Six tetrahedra per complete nominal (T, B, angle) hexahedron, built and
    queried in (T, ln B, angle) coordinates; ln-Ic interpolated, then
    exponentiated. Re-implements the *rule* described in the contract for
    crates/optcoil-physics/src/critical_current.rs; no code or search order
    is ported from that file, and it is never imported here."""

    def __init__(self, points, limits):
        self.points = points
        self._exact = {actual_position(p): p["ic_a_per_m"] for p in points}
        axes = [sorted({nominal_position(p)[k] for p in points}) for k in range(3)]
        if any(len(a) < 2 for a in axes):
            raise ValueError("each interpolation axis needs at least two nominal levels")
        self.axes = axes
        index_of = [{v: i for i, v in enumerate(a)} for a in axes]
        lookup = {}
        for idx, p in enumerate(points):
            pos = nominal_position(p)
            key = tuple(index_of[k][pos[k]] for k in range(3))
            if key in lookup:
                raise ValueError("duplicate nominal node")
            lookup[key] = idx
        sizes = [len(a) for a in axes]
        candidate_cells = 0
        span_rejected = 0
        missing_corner = 0
        supported = 0
        tetrahedra = []
        for t in range(sizes[0] - 1):
            for b in range(sizes[1] - 1):
                for ang in range(sizes[2] - 1):
                    candidate_cells += 1
                    base = (t, b, ang)
                    scale_lin = [axes[k][base[k] + 1] - axes[k][base[k]] for k in range(3)]
                    field_ratio = axes[1][b + 1] / axes[1][b]
                    if (
                        scale_lin[0] > limits["temperature_k"]
                        or field_ratio > limits["field_ratio"]
                        or scale_lin[2] > limits["angle_deg"]
                    ):
                        span_rejected += 1
                        continue
                    corners = [None] * 8
                    complete = True
                    for bits in range(8):
                        key = tuple(base[k] + ((bits >> k) & 1) for k in range(3))
                        idx = lookup.get(key)
                        if idx is None:
                            complete = False
                        else:
                            corners[bits] = idx
                    if not complete:
                        missing_corner += 1
                        continue
                    coordinate_scale = (scale_lin[0], math.log(field_ratio), scale_lin[2])
                    for perm in PERMUTATIONS:
                        first = 1 << perm[0]
                        second = first | (1 << perm[1])
                        nodes = (corners[0], corners[first], corners[second], corners[7])
                        actual = [self._log_coords(actual_position(points[i])) for i in nodes]
                        nominal = [self._log_coords(nominal_position(points[i])) for i in nodes]
                        tetrahedra.append(self._build_tetra(nodes, actual, nominal, coordinate_scale))
                    supported += 1
        if not tetrahedra:
            raise ValueError("no complete interpolation cells within the declared span limits")
        self.tetrahedra = tetrahedra
        self.summary = {
            "training_points": len(points),
            "nominal_axis_sizes": sizes,
            "candidate_cells": candidate_cells,
            "supported_cells": supported,
            "missing_corner_cells": missing_corner,
            "span_rejected_cells": span_rejected,
            "tetrahedra": len(tetrahedra),
        }

    @staticmethod
    def _log_coords(position):
        return (position[0], math.log(position[1]), position[2])

    @staticmethod
    def _build_tetra(nodes, actual, nominal, scale):
        def edges(pts):
            return [tuple((pts[i + 1][k] - pts[0][k]) / scale[k] for k in range(3)) for i in range(3)]

        actual_edges = edges(actual)
        nominal_edges = edges(nominal)
        determinant = dot(actual_edges[0], cross(actual_edges[1], actual_edges[2]))
        nominal_determinant = dot(nominal_edges[0], cross(nominal_edges[1], nominal_edges[2]))
        if not math.isfinite(determinant) or abs(determinant) < 1e-10 or determinant * nominal_determinant <= 0.0:
            raise ValueError("measured coordinates create a degenerate or inverted tetrahedron")
        inverse = [
            tuple(x / determinant for x in cross(actual_edges[1], actual_edges[2])),
            tuple(x / determinant for x in cross(actual_edges[2], actual_edges[0])),
            tuple(x / determinant for x in cross(actual_edges[0], actual_edges[1])),
        ]
        low = tuple(min(p[k] for p in actual) for k in range(3))
        high = tuple(max(p[k] for p in actual) for k in range(3))
        return {
            "nodes": nodes,
            "origin": actual[0],
            "scale": scale,
            "inverse": inverse,
            "low": low,
            "high": high,
        }

    def evaluate(self, position):
        """position = (T, applied_field_t, angle_from_normal_deg).

        Returns (ic_a_per_m, exact_measurement) or None -- None means outside
        the union of supported measured cells (including interior holes), not
        a zero or infeasible current. No nearest-neighbor fallback, no
        extrapolation, ever."""
        exact = self._exact.get(tuple(position))
        if exact is not None:
            return exact, True
        if position[1] == 0.0:
            return None
        pos = self._log_coords(position)
        for tetra in self.tetrahedra:
            scale, low, high = tetra["scale"], tetra["low"], tetra["high"]
            inside = True
            for k in range(3):
                if pos[k] < low[k] - scale[k] * CONTAINMENT_ROUNDOFF or pos[k] > high[k] + scale[k] * CONTAINMENT_ROUNDOFF:
                    inside = False
                    break
            if not inside:
                continue
            origin = tetra["origin"]
            relative = tuple((pos[k] - origin[k]) / scale[k] for k in range(3))
            tail = [dot(row, relative) for row in tetra["inverse"]]
            weights = [1.0 - sum(tail), tail[0], tail[1], tail[2]]
            if any(
                (not math.isfinite(w)) or w < -CONTAINMENT_ROUNDOFF or w > 1.0 + CONTAINMENT_ROUNDOFF
                for w in weights
            ):
                continue
            weights = [min(1.0, max(0.0, w)) for w in weights]
            total = sum(weights)
            weights = [w / total for w in weights]
            ln_ic = sum(w * math.log(self.points[i]["ic_a_per_m"]) for w, i in zip(weights, tetra["nodes"]))
            ic = math.exp(ln_ic)
            if not math.isfinite(ic) or ic <= 0.0:
                raise ValueError("nonfinite or nonpositive interpolated current")
            return ic, False
        return None


def query_k(interpolator, temperature_k, magnitude_t, angle_deg, low_field_clamp_t):
    """A8 order of operations for one mirror angle, applied to each
    period-180 representation of the angle in turn (angle, then angle - 180;
    contract A5's field-reversal symmetry: the measured 0-degree plane sits
    at slightly negative Hall angles, so orientations that fold to just
    below 180 degrees are covered only in their negative-angle form).
    Returns (k_a_per_m_or_None, basis, query_field_t_attempted)."""
    for representation in (angle_deg, angle_deg - 180.0):
        result = interpolator.evaluate((temperature_k, magnitude_t, representation))
        if result is not None:
            return result[0], "estimate", magnitude_t
        if magnitude_t < low_field_clamp_t:
            result = interpolator.evaluate((temperature_k, low_field_clamp_t, representation))
            if result is not None:
                return result[0], "lower_bound", low_field_clamp_t
    return None, "unsupported", magnitude_t


# ---------------------------------------------------------------------------
# Main
# ---------------------------------------------------------------------------


def build_reference(case, csv_bytes, metadata, case_raw_bytes, orders, evaluated_current_a):
    material = case["material"]
    if metadata["id"] != material["dataset_id"]:
        raise ValueError("case material.dataset_id does not match the supplied metadata id")
    if sha256_bytes(csv_bytes) != material["csv_sha256"] or sha256_bytes(csv_bytes) != metadata["csv_sha256"]:
        raise ValueError("measurements CSV SHA-256 does not match the case or its metadata")
    if metadata["electric_field_criterion_v_per_m"] != case["operating"]["electric_field_criterion_v_per_m"]:
        raise ValueError("case electric_field_criterion_v_per_m does not match the dataset metadata")

    points = read_material_points(csv_bytes)
    interpolator = MeasuredCoordinateLogInterpolator(points, metadata["max_cell_spans"])

    pack = case["pack"]
    winding = case["winding"]
    operating = case["operating"]
    temperature_k = operating["temperature_k"]
    if not (metadata["selection"]["nominal_temperature_k"][0] < temperature_k < metadata["selection"]["nominal_temperature_k"][-1]):
        raise ValueError("operating temperature is not strictly interior to the dataset's nominal span")
    if evaluated_current_a not in operating["current_candidates_a"]:
        raise ValueError("evaluated_current_a must be one of the case's current candidates")
    low_field_clamp_t = material["low_field_clamp_t"]

    if winding["tape_normal"] == "radial":
        fill = winding["tapes_along_width"] * winding["tape_width_m"]
        extent = pack["axial_height_m"]
    else:
        fill = winding["tapes_along_width"] * winding["tape_width_m"]
        extent = pack["radial_width_m"]
    if abs(fill / extent - 1.0) > 1e-9:
        raise ValueError("tapes_along_width * tape_width_m does not fill the declared pack extent")

    total_turns = winding["turns_along_normal"] * winding["tapes_along_width"]

    stations_by_id = {s["id"]: s for s in case["sampling"]["stations"]}
    valid_turns = set(case["sampling"]["turn_indices_along_normal"])
    valid_tapes = set(case["sampling"]["tape_indices_along_width"])

    # OC-014 (schema v2): when the case declares
    # limits.self_field_correction = "uniform_transport", every query
    # magnitude is |B_applied| + mu0*K_tape/2 — the uniform-sheet
    # self-field bound, K_tape = I/(w*s) being the carried sheet density
    # of one strand.
    correction = case["limits"].get("self_field_correction")
    strands = float(winding.get("strands_parallel", 1))
    k_transport = evaluated_current_a / (winding["tape_width_m"] * strands)

    # OC-014 Phase 2b: the case-level suffix-max all-angle bounding table
    # for the field-consistent critical-state solve — built once, shared
    # across every point.
    cs_table = None
    if correction == "critical_state_strip":
        cs_table = build_critical_state_table(interpolator, temperature_k, low_field_clamp_t)

    points_out = []
    for entry in case["numerics"]["reference_subset"]:
        station = stations_by_id.get(entry["station"])
        if station is None:
            raise ValueError(f"reference_subset station {entry['station']!r} is not sampled")
        if entry["tape_index"] not in valid_tapes or entry["turn_index"] not in valid_turns:
            raise ValueError("reference_subset entry references an undeclared tape/turn index")
        frame = station_frame(station, pack["straight_half_length_m"])
        for width_index, xi in enumerate(GL5_NODES):
            position, n_hat, w_hat, t_hat = tape_position(
                pack, winding, frame, entry["turn_index"], entry["tape_index"], xi
            )
            unit_fields = []
            for order in orders:
                geometry = dict(pack, ampere_turns_a=1.0)
                unit_fields.append(field_graded(geometry, position, order))
            field_t_vec = unit_fields[-1] * (total_turns * evaluated_current_a)
            magnitude = float(np.linalg.norm(field_t_vec))
            b_n = float(dot(field_t_vec, n_hat))
            b_w = float(dot(field_t_vec, w_hat))
            b_t = float(dot(field_t_vec, t_hat))
            theta_raw = math.degrees(math.atan2(b_w, b_n))
            theta_folded = fold_angle(theta_raw)
            theta_mirror = mirror_angle(theta_folded)
            along_fraction = abs(b_t) / magnitude if magnitude > 0.0 else 0.0

            # OC-014 Phase 2b: under "critical_state_strip" the query
            # magnitude carries the field-consistent critical-state edge
            # bound — the largest fixed point of
            # B_edge = (mu0/2pi)*k_bound(|B_app|+B_edge)*ln(w/d) over the
            # suffix-max all-angle table built once per case, valid at any
            # transport ratio. If the table failed to build the self-field
            # is unbounded and the point is unsupported, never an
            # uncorrected-field pass.
            cs_unboundable = False
            if correction == "critical_state_strip":
                layer_thickness = case["limits"]["critical_state_layer_thickness_m"]
                if cs_table is None:
                    cs_unboundable = True
                    query_magnitude = magnitude
                else:
                    query_magnitude = magnitude + cs_consistent_edge_field(
                        cs_table,
                        magnitude,
                        winding["tape_width_m"],
                        layer_thickness,
                        low_field_clamp_t,
                    )
            else:
                query_magnitude = magnitude + (MU0_H_PER_M * k_transport / 2.0 if correction else 0.0)

            # A5 along-current exclusion: gate BEFORE querying either mirror
            # angle, mirroring crates/optcoil-physics/src/tape_frame.rs's
            # evaluate_candidate_point (query_field_t is the raw magnitude
            # here, not the low-field clamp -- the clamp sequence is never
            # entered for an excluded point).
            # OC-017: under limits.along_current_model = "transverse_bound"
            # (schema v3) an over-limit point at or below the bound ceiling
            # still runs the measured-law query and is labeled
            # "along_current_bounded" instead of excluded.
            along_bounded = (
                case["limits"].get("along_current_model") == "transverse_bound"
                and along_fraction > case["limits"]["max_along_current_field_fraction"]
                and along_fraction <= ALONG_CURRENT_BOUND_CEILING
            )
            if along_fraction > case["limits"]["max_along_current_field_fraction"] and not along_bounded:
                point_basis = "along_current_excluded"
                query_field_t = magnitude
                k_fold = k_mirror = k_used = None
            else:
                k_fold, basis_fold, _ = query_k(interpolator, temperature_k, query_magnitude, theta_folded, low_field_clamp_t)
                k_mirror, basis_mirror, _ = query_k(
                    interpolator, temperature_k, query_magnitude, theta_mirror, low_field_clamp_t
                )

                if basis_fold == "unsupported" or basis_mirror == "unsupported":
                    underlying_basis = "unsupported"
                elif basis_fold == "lower_bound" or basis_mirror == "lower_bound":
                    underlying_basis = "lower_bound"
                else:
                    underlying_basis = "estimate"
                # The bounded label overrides the underlying basis in the
                # record only when the bound produced a value; query_field_t
                # still follows the lower-bound clamp, matching the Rust
                # combine_mirror_pair ordering. A tilted point whose query
                # produced no k keeps its underlying basis — there is
                # nothing to bound, and the record must show the data gap.
                query_field_t = low_field_clamp_t if underlying_basis == "lower_bound" else query_magnitude
                k_used = min(k_fold, k_mirror) if (k_fold is not None and k_mirror is not None) else None
                point_basis = (
                    "along_current_bounded"
                    if along_bounded and k_used is not None and underlying_basis in ("estimate", "lower_bound")
                    else underlying_basis
                )
                # critical_state_strip with no resolvable floor sheet current
                # cannot bound the self-field: the point is unsupported.
                if cs_unboundable:
                    k_used = None
                    point_basis = "unsupported"
                    query_field_t = magnitude

            points_out.append(
                {
                    "station": entry["station"],
                    "tape_index": entry["tape_index"],
                    "turn_index": entry["turn_index"],
                    "width_index": width_index,
                    "position_m": list(position),
                    "unit_fields_t_per_ampere_turn": [list(v) for v in unit_fields],
                    "field_t": list(field_t_vec),
                    "magnitude_t": magnitude,
                    "angle_raw_deg": theta_raw,
                    "angle_folded_deg": theta_folded,
                    "mirror_angle_deg": theta_mirror,
                    "along_current_fraction": along_fraction,
                    "query_field_t": query_field_t,
                    "basis": point_basis,
                    "k_folded_a_per_m": k_fold,
                    "k_mirror_a_per_m": k_mirror,
                    "k_used_a_per_m": k_used,
                }
            )
    return points_out


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--case", type=Path, default=ROOT / "benchmarks/coupled/oc-004.json")
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument(
        "--csv",
        type=Path,
        default=ROOT / "data/materials/robinson-superpower-ap-v3/measurements.csv",
    )
    parser.add_argument(
        "--metadata",
        type=Path,
        default=ROOT / "data/materials/robinson-superpower-ap-v3/material.json",
    )
    parser.add_argument("--orders", default="24,48")
    parser.add_argument("--evaluated-current-a", type=float, default=870.0)
    args = parser.parse_args()

    orders = [int(x) for x in args.orders.split(",")]
    if len(orders) != 2 or orders[0] >= orders[1]:
        raise ValueError("--orders must be exactly two strictly increasing integers")

    case_raw = args.case.read_bytes()
    case = json.loads(case_raw)
    if case["schema"] not in (
        "optcoil-coupled-conductor/v1",
        "optcoil-coupled-conductor/v2",
        "optcoil-coupled-conductor/v3",
        "optcoil-coupled-conductor/v4",
    ):
        raise ValueError("unsupported case schema")
    limits = case.get("limits", {})
    if (limits.get("self_field_correction") == "critical_state_strip") != (
        "critical_state_layer_thickness_m" in limits
    ):
        raise ValueError(
            "critical_state_strip and critical_state_layer_thickness_m must be declared together"
        )
    csv_bytes = args.csv.read_bytes()
    metadata = json.loads(args.metadata.read_text())

    started = time.perf_counter()
    points_out = build_reference(case, csv_bytes, metadata, case_raw, orders, args.evaluated_current_a)
    elapsed = time.perf_counter() - started

    record = {
        "schema": "optcoil-coupled-reference/v1",
        "case_sha256": sha256_bytes(case_raw),
        "method": (
            METHOD_CRITICAL_STATE_ALONG_CURRENT
            if case["limits"].get("self_field_correction") == "critical_state_strip"
            and case["limits"].get("along_current_model")
            else METHOD_CRITICAL_STATE
            if case["limits"].get("self_field_correction") == "critical_state_strip"
            else METHOD_V3
            if case["limits"].get("self_field_correction")
            and case["limits"].get("along_current_model")
            else METHOD_V2
            if case["limits"].get("self_field_correction")
            else METHOD_ALONG_CURRENT
            if case["limits"].get("along_current_model")
            else METHOD
        ),
        "source_sha256": sha256_bytes(Path(__file__).read_bytes()),
        "field_source_sha256": sha256_bytes((ROOT / "tools/reference_oc002.py").read_bytes()),
        "source_path": "tools/reference_oc004.py",
        "csv_sha256": sha256_bytes(csv_bytes),
        "python_version": platform.python_version(),
        "numpy_version": np.__version__,
        "platform": platform.platform(),
        "orders": orders,
        "component_subdivisions": 16,
        "mu0_h_per_m": MU0_H_PER_M,
        "evaluated_current_a": args.evaluated_current_a,
        "points": points_out,
        "elapsed_seconds": elapsed,
        "limitations": [
            "Screening reference only: not a production operating-current limit.",
            "Independent NumPy/pure-Python implementation; reuses only OC-002's field-kernel "
            "building blocks (cells/planar_source/projected_parameters), never the Rust "
            "interpolator or evaluator. Field evaluation uses this file's own field_graded, "
            "not reference_oc002.field directly: near tape centers, a Duffy triangle's "
            "transverse coordinate can have a short-to-long extent ratio far below what a "
            "plain order-point Gauss-Legendre rule resolves, so that coordinate is refined "
            "with a geometrically graded composite rule (panels [0,r],[r,4r],... on [0,1], "
            "engaged when r < 0.25) while the dominant coordinate keeps the unchanged plain "
            "rule; see the module docstring and docs/OC004.md.",
            "The point-level 'basis' and 'query_field_t' fields are an aggregate over the two mirror "
            "angles per the contract's point-basis rule; k_folded_a_per_m/k_mirror_a_per_m are exact "
            "per-angle results (see module docstring).",
            "No monotonicity audit is performed by this tool; the low-field clamp is applied "
            "mechanically per the contract's declared order of operations.",
            "Applied-field bridge law includes the measurement sample's own self-field response; "
            "not an intrinsic local Jc(B) law (inherited from the OC-003 dataset).",
            "Homogenized racetrack pack, prescribed uniform tangential current; no screening "
            "currents, redistribution, or iron (inherited from the OC-002 field model).",
        ],
    }
    if args.output.exists():
        raise FileExistsError(f"refusing to overwrite existing reference at {args.output}")
    with args.output.open("x") as handle:
        json.dump(record, handle, indent=2, allow_nan=False)
        handle.write("\n")
    print(
        f"Wrote {args.output}; {len(points_out)} points; elapsed {elapsed:.3f}s "
        f"(orders={orders}, evaluated_current_a={args.evaluated_current_a})"
    )


if __name__ == "__main__":
    main()
