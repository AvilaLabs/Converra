#!/usr/bin/env python3
"""Prepare the separately reserved 45 K validation challenge from the same source.

The original 20-40 K dataset and its preparation script remain unchanged.
The challenge is fixed before assessing the logarithmic model: train on nominal
20,25,30,35,40,50 K and hold out the entire 45 K plane.
"""
import argparse
import hashlib
import json
from pathlib import Path

import prepare_oc003 as base


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output-dir", type=Path, required=True)
    args = parser.parse_args()
    base.TEMPERATURES = [20.0, 25.0, 30.0, 35.0, 40.0, 45.0, 50.0]
    artifacts = base.prepare()
    metadata = json.loads(artifacts["material.json"])
    metadata["id"] = "robinson-superpower-ap-v3-challenge-20-50k"
    metadata["preparation_source_sha256"] = hashlib.sha256(Path(base.__file__).read_bytes() + Path(__file__).read_bytes()).hexdigest()
    artifacts["material.json"] = (json.dumps(metadata, indent=2) + "\n").encode()
    args.output_dir.mkdir(parents=True, exist_ok=True)
    for name in artifacts:
        if (args.output_dir / name).exists():
            raise FileExistsError(args.output_dir / name)
    for name, content in artifacts.items():
        with (args.output_dir / name).open("xb") as output:
            output.write(content)
    print(json.dumps({"points": metadata["point_count"], "csv_sha256": metadata["csv_sha256"]}, indent=2))


if __name__ == "__main__":
    main()
