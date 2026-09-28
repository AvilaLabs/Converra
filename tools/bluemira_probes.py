#!/usr/bin/env python3
"""OC-011 kernel cross-check runner: Bluemira thin-filament Biot-Savart
field answers for a frozen OptCoil probe deck.

Usage: bluemira_probes.py <deck.json> <out.json>

Reads the deck (schema optcoil-kernel-probe-deck/v1), then builds the
source named by the deck's `evaluation_model`:

- `thin_filament` (OC-011 Phase A): the racetrack centerline as a
  Biot-Savart filament (two straights along x joined by semicircular
  arcs of the centerline bend radius, loop in the z=0 plane).
- `finite_cross_section` (Phase B): `ArbitraryPlanarPolyhedralXSCircuit`
  — the same centerline polygon swept with the real pack cross-section
  (radial_width_m x axial_height_m) as mitered trapezoidal-prism
  segments with uniform current density NI / (w * h). This is Bluemira's
  independently implemented finite-XSC model (Fabbri's analytic surface
  integrals), cross-checked against OptCoil's volume-quadrature cell
  evaluator on the Rust side.

Sets current = ampere_turns_a and writes the field at every probe as
canonical JSON. OptCoil's verdict logic lives entirely on the Rust
side; this script only answers "what is B at these points" in
Bluemira's own implementation.

L == 0 degenerates the racetrack into a full circular loop of radius R
(two semicircles back-to-back), which is how the runner is validated
against the in-tree closed form before the racetrack comparison is
trusted.
"""

import hashlib
import json
import math
import sys


def centerline_nodes(geom, arc_segments=256, points_per_metre=200):
    """Closed centerline polygon for the racetrack in the z=0 plane,
    wound in OptCoil's convention: bottom straight (-l,-R)->(+l,-R),
    right semicircle bulging +x, top straight (+l,+R)->(-l,+R), left
    semicircle bulging -x — counterclockwise seen from +z, so +NI gives
    +B_z inside the loop (matching the cell evaluator's part tangents).
    """
    l = geom["straight_half_length_m"]
    r = geom["centerline_bend_radius_m"]
    nodes = []

    n_arc = max(8, int(arc_segments))
    straight_len = 2.0 * l
    n_straight = max(2, int(math.ceil(straight_len * points_per_metre)))

    # Bottom straight: (-l, -R) -> (+l, -R) — skipped entirely when l==0
    # (the racetrack degenerates to a full loop; zero-length straights
    # would inject duplicate nodes and NaN segments).
    if straight_len > 0.0:
        for i in range(n_straight):
            nodes.append([-l + straight_len * i / n_straight, -r, 0.0])
    # Right arc: (l, -R) -> (l, +R), centre (l, 0), angle -pi/2 -> +pi/2
    for i in range(n_arc + 1):
        a = -math.pi / 2 + math.pi * i / n_arc
        nodes.append([l + r * math.cos(a), r * math.sin(a), 0.0])
    # Top straight: (+l, +R) -> (-l, +R)
    if straight_len > 0.0:
        for i in range(1, n_straight):
            nodes.append([l - straight_len * i / n_straight, r, 0.0])
    # Left arc: (-l, +R) -> (-l, -R), centre (-l, 0), +pi/2 -> +3pi/2
    for i in range(1, n_arc + 1):
        a = math.pi / 2 + math.pi * i / n_arc
        nodes.append([-l + r * math.cos(a), r * math.sin(a), 0.0])

    # Drop consecutive duplicates (arc/straight junctions can coincide).
    deduped = []
    for n in nodes:
        if not deduped or any(abs(n[k] - deduped[-1][k]) > 1e-12 for k in range(3)):
            deduped.append(n)
    return deduped


def main():
    if len(sys.argv) != 3:
        print("usage: bluemira_probes.py <deck.json> <out.json>", file=sys.stderr)
        return 2

    deck_path, out_path = sys.argv[1], sys.argv[2]
    with open(deck_path) as f:
        deck = json.load(f)
    if deck.get("schema") != "optcoil-kernel-probe-deck/v1":
        print(f"unsupported deck schema: {deck.get('schema')}", file=sys.stderr)
        return 2

    model = deck.get("evaluation_model", "thin_filament")
    try:
        import numpy as np

        import bluemira  # noqa: F401
        from bluemira.geometry.coordinates import Coordinates
    except ImportError as e:
        print(f"bluemira import failed: {e}", file=sys.stderr)
        return 3

    nodes = centerline_nodes(deck["geometry"])
    if model == "thin_filament":
        from bluemira.magnetostatics.biot_savart import BiotSavartFilament

        # BiotSavartFilament takes the closed polygon's nodes as
        # Coordinates; `field` is a classmethod over probe arrays.
        coords = Coordinates(np.asarray(nodes, dtype=float))
        source = BiotSavartFilament(
            coords, radius=1e-6, current=deck["ampere_turns_a"]
        )
    elif model == "finite_cross_section":
        from bluemira.magnetostatics.circuits import (
            ArbitraryPlanarPolyhedralXSCircuit,
        )

        # Sweep the real pack cross-section along the same centerline
        # polygon. Bluemira mitres each segment's end caps to the path's
        # half-angles, so the fan of trapezoidal prisms tiles the swept
        # annular region; the residual vs the true arc is the polygon's
        # chord sagitta — the same discretization class as Phase A's
        # filament. Local x of the XSC maps to t_vec = ds x normal
        # (radially outward for our z=0 winding) and local z to the
        # shape normal (+z axial), so the rectangle is
        # radial_width x axial_height. The deck's tape_normal does not
        # change the swept volume — uniform J fills the same pack either
        # way.
        geom = deck["geometry"]
        w2 = geom["radial_width_m"] / 2.0
        h2 = geom["axial_height_m"] / 2.0
        # `centerline_nodes` already returns a closed loop — the left
        # arc's last point coincides with the bottom straight's first
        # (up to last-ulp rounding). Snap it to the exact first point:
        # Bluemira's `closed` check uses atol=0, and appending a
        # duplicate instead would make a zero-length prism segment.
        nodes[-1] = nodes[0]
        path = Coordinates(np.asarray(nodes, dtype=float))
        xs = Coordinates(
            np.array(
                [
                    [-w2, w2, w2, -w2],
                    [0.0, 0.0, 0.0, 0.0],
                    [-h2, -h2, h2, h2],
                ],
                dtype=float,
            )
        )
        source = ArbitraryPlanarPolyhedralXSCircuit(
            path, xs, current=deck["ampere_turns_a"]
        )
    else:
        print(f"unsupported evaluation_model: {model}", file=sys.stderr)
        return 2

    probes = np.asarray(deck["probes_m"], dtype=float)
    bx, by, bz = source.field(probes[:, 0], probes[:, 1], probes[:, 2])
    b_out = [
        [float(x), float(y), float(z)]
        for x, y, z in zip(np.atleast_1d(bx), np.atleast_1d(by), np.atleast_1d(bz))
    ]

    response = {
        "solver_name": "bluemira",
        "solver_version": getattr(bluemira, "__version__", "unknown"),
        "b_t": b_out,
    }
    with open(out_path, "w") as f:
        json.dump(response, f, sort_keys=True)
        f.write("\n")
    # Emit the request hash so the caller can bind response to request.
    with open(deck_path, "rb") as f:
        print("request_sha256:", hashlib.sha256(f.read()).hexdigest())
    return 0


if __name__ == "__main__":
    sys.exit(main())
