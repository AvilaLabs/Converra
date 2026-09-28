# Robinson Research Institute / Faraday Factory Japan YBCO, version 1

Third-party measurement data, **CC BY 4.0**. Converra is MIT-licensed; that license covers project source and does not replace or extend to these data.

Attribution: Andres Pantoja, Stuart Wimbush and Nick Strickland, *Critical current characterisation of Faraday Factory Japan YBCO 2G HTS superconducting wire*, Robinson Research Institute, Victoria University of Wellington. [Dataset DOI, version 4](https://doi.org/10.6084/m9.figshare.23536374.v4). [CC BY 4.0 license](https://creativecommons.org/licenses/by/4.0/). Downloaded September 16, 2026.

The published workbook and description are not vendored; fetch them from the dataset DOI above (`Description.txt` uses Windows-1252 encoding). The source workbook SHA-256 is pinned in the preparation tool, which expects it at `source/All data.xlsx` if you re-run preparation.

The generated `measurements.csv`, `material.json` and `preparation-audit.json` are Avila Labs' attributed adaptations. The preparation tool selects nominal 20–40 K — the identical temperature window as `robinson-superpower-ap-v3`, `robinson-shanghai-hflt-v3` and `robinson-theva-ap-v2`, so all four datasets cover the same operating region — across this dataset's full nonzero field grid (0.01–8 T, including a dense 0.01–0.7 T decade the peer datasets do not measure) and 0–180°; converts published A/cm to A/m; preserves measured temperature, source field and Hall angle; and records every excluded source row. The 0 T rows are self-field measurements and are excluded — there is no positive applied field to interpolate against. Full-turn angle unwrapping aligns coordinates with the commanded angle without assuming reflection symmetry. No measurement values are fit, averaged or invented during preparation. See `tools/prepare_ffj_ybco.py` for the exact transformation.

The tested specimen is a **0.5 mm × 5 mm patterned bridge** made from a 4 mm-wide tape, sample FDY003 / manufacturer designation #1043-N1 (measured September–October 2022). The criterion is 0.5 µV over 5 mm, equivalent to 1 µV/cm — the same instrument, criterion and conventions as the other Robinson datasets. Width-normalized current does not establish the capacity of an unpatterned full-width tape, and this is a 4 mm product — per-width capacity does not account for width-dependent slitting or substrate differences in other widths. Applied-field data also includes the bridge's self-field response; it cannot silently become a local intrinsic critical-current law for OptCoil's winding-pack field.

This source measures a denser angle set near the ab-plane peak — 1–2° setpoints across 75–105° against 5° elsewhere (plus a 185–240° back-quadrant the selection excludes) — and a much denser low-field decade than the peer datasets. The prepared mesh keeps those nodes: a denser measured mesh, not a different declared window.

`bundle.json` is an unsigned optcoil-material-dataset/v2 bundle; it has no `attestation` member until it is signed with the `avila-labs` issuer key.

No manufacturer endorsement, lot guarantee, quantified uncertainty, strain coverage or engineering acceptance is implied.
