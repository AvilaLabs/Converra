# Your first supported study

Start with the workbench's **Measured conductor first study** or
`benchmarks/coupled/first-study.json`. It uses attributed Robinson SuperPower
AP data at 21 K, a small racetrack candidate set and **invented teaching prices**.
The geometry and costs are an example, not a customer's design or supplier quote.

## Review applicability

In the workbench, expand **Applicability and work estimate** before running.
Review geometry, dataset identity and evidence class, width assumptions, operating
criterion, price provenance, and missing screens. The work estimate counts
sampling work; it is not an elapsed-time guarantee or a coverage verdict.

From the repository root:

```bash
cargo run --release -- preflight benchmarks/coupled/first-study.json
cargo run --release -- coupled-search benchmarks/coupled/first-study.json \
  --output runs/first-study.json
```

The small first study deliberately exposes a limit of the uncorrected self-field
screen: its candidates remain `INCONCLUSIVE` when the limiting self-field ratio
exceeds the declared bound. No candidate can be recommended from that result.
The CLI saves the diagnostic record and exits nonzero because the search is not
fully PASS. Read **Decision at declared inputs and prices** and the named next
steps. Raising a bound merely to get PASS would change the engineering question.

For a larger reference with a documented passing option, use
`benchmarks/coupled/oc-007.json`; its 15-candidate search and refined checks take
longer. See [OC-007](OC007.md) for its exact scope and results.

## Revise and compare

Use **Revise** to edit the complete loaded JSON, or **File → Duplicate current
case** to retain every declaration with a new identity. Accepting a revision
validates it first. Save to a new filename; source files are protected. Cancelling
or an invalid edit retains the current case and completed result. The previous
completed record remains available for comparison after an accepted revision.

Use the Reports view to compare changed inputs, dataset identities and results.
A change in requirements, material policy or fidelity can explain a cost change;
matching dollar figures alone do not establish an equivalent design comparison.

A scalar price scenario is available only for uniform pricing. Graded and
piece-priced cases require editing the per-spec declaration and rerunning. A
scenario's cheapest screening candidate has `NOT_EVALUATED` selection acceptance;
the original refined recommendation stays attached to the original record.

To compare two explicit alternatives, open **Engineering study**, keep the
first-study case as the baseline, and duplicate it as a named variant. Revise
one declared choice or requirement on the variant, review its applicability,
and run it. The study comparison shows both input differences and their own
calculated decisions; an unrun variant has no result to compare.

## Bring conductor evidence

Use **File → Import spreadsheet / CSV** to import a UTF-8 CSV, TSV or Excel XLSX
table. The import flow previews the source, lets you choose a worksheet when
needed, and requires explicit mappings for temperature, applied field, angle,
critical current and either an n-value column or a declared constant. Choose
units for each measured quantity. Supply nominal coordinates as columns or
explicitly select **use measured coordinates as the nominal grid**; commanded
and measured coordinates are not assumed to be interchangeable.

The source step asks for attribution, usage terms, material identity and
measurement declarations. You do not need to create metadata JSON first. The
workbench records the uploaded file hash and mapping/unit receipt, converts to
canonical SI data, and applies the same material validation used by other
datasets. Ic per width uses the declared **measured bridge width** to derive
bridge current; original tape width does not imply full-width capacity. The
importer does not fill missing attribution, infer unsupported measurement
conventions, hide duplicate coordinates or extrapolate outside the observed
domain. Files are bounded to 32 MiB; malformed or unsupported tables must be
corrected or converted before import. A validated import can be applied to a
case draft or exported as a dataset bundle.

The Materials view also accepts an existing dataset bundle or an attributed
metadata JSON plus canonical measurement CSV. The CSV hash is computed when
metadata omits it or declares null; an existing mismatched hash is rejected.
For a graded case, load every external dependency shown on Materials. Each
source must match its declared id and CSV hash. The workbench retains matching
sources when you revise the case and uses all of them in searches and margin
sweeps. See [datasets](DATASETS.md) for canonical columns and provenance.

Document a price category and its source or assumptions. Placeholder prices,
published estimates and actual supplier quotes remain distinguishable. A price
label alone does not verify a quote or reproduce commercial terms.

## Export and verify

**Reports → Review package** exports exact case bytes, the completed record,
readable HTML and decision summary, material bundles, an artifact hash manifest,
and BOM/RFQ when a selected option exists. A study without an optimum carries an
explicit procurement-unavailable note. Desktop creates a new package directory;
browser downloads a tar archive that preserves its directory layout.

```bash
cargo run --release -- review-package runs/first-study.json \
  benchmarks/coupled/first-study.json --output runs/first-study-review
cargo run --release -- verify-package runs/first-study-review
```

A saved record alone cannot recreate the original case bytes. Use **Attach
original case** in Reports to bind the source file by its recorded SHA-256 before
exporting a rerunnable package. External conductor bundles must also be available.
The package README includes a rerun command with repeated `--dataset-bundle`
flags for all dependencies. Run it from the package directory after extracting
an archive; the original input directory is not required.

Verification checks hashes, identities and modeled ledger arithmetic. Cost
recomputation is separate from search; its shared physics is not independent
physical validation. Structural, thermal, quench-protection, manufacturing and
full-width conductor qualification still need engineering evidence.

## Validation beyond this example

A reference workflow verifies software behavior. Product validation additionally
needs an identified engineer, their authorized case and conductor data, documented
prices, an actual decision and observed effort compared with their existing process.
That external gate remains open until those observations exist.
