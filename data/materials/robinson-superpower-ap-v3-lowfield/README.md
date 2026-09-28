# Robinson Research Institute / SuperPower AP, version 3 -- low-field extension

Third-party measurement data, **CC BY 4.0**. Converra is MIT-licensed; that license covers project source and does not replace or extend to these data.

Attribution: Stuart Wimbush, Nick Strickland and Andres Pantoja, *Critical current characterisation of SuperPower Advanced Pinning 2G HTS superconducting wire*, Robinson Research Institute, Victoria University of Wellington. [Dataset DOI, version 3](https://doi.org/10.6084/m9.figshare.4256624.v3). [CC BY 4.0 license](https://creativecommons.org/licenses/by/4.0/). Same archived workbook as `data/materials/robinson-superpower-ap-v3/`, downloaded September 9, 2026.

## What this is

This directory extends `robinson-superpower-ap-v3` downward in applied field, for OC-006. The original dataset directory is untouched and stays byte-identical; it remains the dataset pinned by the frozen OC-003/OC-004/OC-005 benchmarks. This is a separate, independently identified dataset (`robinson-superpower-ap-v3-lowfield`) produced by a new tool, `tools/prepare_oc006.py`, which imports `tools/prepare_oc003.py`'s parser and transformation functions unmodified and changes only which Set field levels are selected. See `tools/prepare_oc006.py` for the exact transformation.

## What differs from the original dataset

- Selection adds nominal Set field 0.05, 0.07, 0.1, 0.15, 0.2, 0.3, 0.5 and 0.7 T to the original 1, 1.5, 2, 3, 5, 7 and 8 T, for 15 nominal field levels total (nominal temperature 20-40 K and angle 0-180 deg are unchanged). 3,225 rows (5 x 15 x 43), versus the original's 1,505 (5 x 7 x 43).
- Every row at Set field >= 1 T is byte-identical (source row, all coordinates, `ic_a_per_m`, `bridge_ic_a`, `n_value`) to the corresponding row of `robinson-superpower-ap-v3/measurements.csv`; `tools/prepare_oc006.py` asserts this before writing any output, and records the check's result in `preparation-audit.json`.
- `max_cell_spans` is unchanged (10 K, field ratio 2.5, 10 deg). Adjacent-level ratios across the full 15-level nominal grid stay well under that ceiling; the ceiling was already set to cover the wider gaps that appear once individual field planes are withheld for validation, which is unaffected by this extension.
- `limitations` gains the two entries described under Exclusion rule below; every original bridge/self-field/measurement-geometry/width caveat is preserved unchanged.
- `preparation-audit.json` additionally records the unwrapped Hall-angle-minus-set-angle statistics (mean, standard deviation, maximum absolute value) per Set field level for the 20-40 K rows, at every workbook field level from 0 T through 8 T -- including the levels this dataset excludes -- and the result of the >= 1 T identity check.

## Exclusion rule

Set field 0.01, 0.015, 0.02 and 0.03 T are excluded: below 0.05 T the unwrapped Hall angle drifts away from the commanded angle enough to risk inverting measured-coordinate tetrahedra at the dataset's 2 deg nominal angle spacing (the maximum deviation grows from about 2-3 deg near 0.05-1 T to 9.03 deg at 0.01 T; see the per-field table in `preparation-audit.json`). Set field 0 T is excluded outright: the Hall angle is meaningless there (tens of degrees of scatter), and zero applied field has no image under the logarithmic (ln B) interpolation model this dataset is meant to support. These exclusions are recorded in `preparation-audit.json` alongside every other excluded source row.

No manufacturer endorsement, lot guarantee, quantified uncertainty, strain coverage or engineering acceptance is implied. This dataset is a characterization artifact for OC-006; see `benchmarks/measured/oc-006.json` for the frozen validation benchmark and its pre-declared consequences.
