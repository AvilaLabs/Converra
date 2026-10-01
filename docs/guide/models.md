# Models and supported domains

Converra couples a declared magnetic-field model to conductor critical-current data sampled along the winding. It screens the geometry and operating choices that the case and dataset support.

## Field and material coverage

Planar racetrack, circular and line-and-arc paths have built-in field evaluation. Non-planar paths require a declared map. The finite-cross-section field model assumes its stated current distribution; agreement with another implementation tests that model's calculation.

Measured conductor tables have dataset-specific field, temperature and angular bounds. Full-field magnitude, tape orientation and along-current fraction matter. An opt-in transverse bound is labeled as a bounded estimate and retains its own limits. Queries outside coverage stay unresolved.

Model extensions and published fits support sizing and sensitivity within their declared domains. Their rows are modeled values. Inspect the evidence class rather than interpreting a high-field number as a measured current limit.

## Self-field and additional screens

Self-field policies depend on the case schema and declarations. Older uncorrected screens use a small self-field-ratio bound; newer uniform-transport policies apply a declared correction within their supported regime. A critical-state redistribution boundary can still produce `INCONCLUSIVE`.

First-order Lorentz-load and hoop-stress screens use declared load-path assumptions. Optional thermal-margin, AC-loss and quench screens evaluate configured bounds. They leave unsupported or unperformed engineering work visible.

## Refinement and acceptance

Sampling resolution controls which local extrema a check sees. The acceptance path can evaluate the selected design and baseline with a finer plan. Coarse candidate success therefore does not promise refined success.

Cost recomputation is separate from search, but physical assumptions are shared. Use [independent benchmark comparisons](benchmarks.md) to assess the numerical evidence, then obtain the conductor, structural, cooling, protection and manufacturing evidence needed for your design.

Technical contracts: [supported domains](https://github.com/AvilaLabs/Converra/blob/main/docs/SUPPORTED_DOMAINS.md) and [technical overview](https://github.com/AvilaLabs/Converra/blob/main/docs/TECHNICAL_SUMMARY.md).
