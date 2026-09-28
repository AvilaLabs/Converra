# Robinson Research Institute / Shanghai Superconductor HFLT, version 3

Third-party measurement data, **CC BY 4.0**. Converra is MIT-licensed; that license covers project source and does not replace or extend to these data.

Attribution: Stuart Wimbush, Nick Strickland and Andres Pantoja, *Critical current characterisation of Shanghai Superconductor High Field Low Temperature 2G HTS superconducting wire*, Robinson Research Institute, Victoria University of Wellington. [Dataset DOI, version 3](https://doi.org/10.6084/m9.figshare.5331145.v3). [CC BY 4.0 license](https://creativecommons.org/licenses/by/4.0/). Downloaded September 15, 2026.

The published workbook, description and versioned Figshare metadata are not vendored; fetch them from the dataset DOI above (`Description.txt` uses Windows-1252 encoding). The source workbook MD5 was checked against Figshare's file metadata and its SHA-256 is pinned in the preparation tool, which expects it at `source/All data.xlsx` if you re-run preparation.

The generated `measurements.csv`, `material.json` and `preparation-audit.json` are Avila Labs' attributed adaptations for the second-dataset graded benchmark. The preparation tool selects nominal 20–40 K, 1–8 T and 0–180° measurements — the identical characterization window as `robinson-superpower-ap-v3`, so both datasets cover the same operating region; converts published A/cm to A/m; preserves measured temperature, source field and Hall angle; and records every excluded source row. Full-turn angle unwrapping aligns coordinates with the commanded angle without assuming reflection symmetry. No measurement values are fit, averaged or invented during preparation. See `tools/prepare_shanghai_hflt.py` for the exact transformation.

The tested specimen is a **0.5 mm × 5 mm patterned bridge** made from a 4 mm-wide tape, sample SHS112 / SC8631244-3-2. The criterion is 0.5 µV over 5 mm, equivalent to 1 µV/cm — the same instrument, criterion and conventions as the SuperPower AP dataset. Width-normalized current does not establish the capacity of an unpatterned full-width tape, and this is a 4 mm product — per-width capacity does not account for width-dependent slitting or substrate differences in other widths. Applied-field data also includes the bridge's self-field response; it cannot silently become a local intrinsic critical-current law for OptCoil's winding-pack field.

No manufacturer endorsement, lot guarantee, quantified uncertainty, strain coverage or engineering acceptance is implied.
