# Your first study

Open [the workbench](https://converra.avilalabs.org). Its **Measured conductor first study** uses attributed Robinson SuperPower AP measurements at 21 K and a small racetrack search. Its prices are invented teaching inputs.

## 1. Review the inputs

Expand **Applicability and work estimate**. Read the field and aperture requirements, operating point, material class, width-transfer assumptions, price source and missing screens. Work estimates count sampling effort; they do not promise elapsed time or establish field coverage.

## 2. Calculate and read the decision

Run the study, then read **Decision at declared inputs and prices**. This example deliberately reaches an uncorrected self-field boundary: candidates remain `INCONCLUSIVE`, and the search cannot recommend a design. The diagnosis and named follow-up steps are the useful result.

Keep the declared bounds intact while learning. Raising a bound changes the question being screened.

For an example with a documented passing option, load `benchmarks/coupled/oc-007.json` from the source checkout. It evaluates 15 candidates and takes longer because it also refines the selected geometry and baseline.

## 3. Revise one choice

Use **Revise** or duplicate the case as a named alternative in **Engineering study**. Change one declared price, operating condition or winding choice, validate the revision, then run it. The previous completed result remains available for comparison.

Compare the changed inputs alongside the decisions. Equal costs can hide different requirements; a lower cost can result from relaxed constraints or changed material assumptions.

## 4. Save the study

Export a study workspace to preserve variants and historical results. Use **Reports → Review package** to collect the exact case, completed record, material dependencies and readable report. Browser export produces a tar archive; desktop creates a new directory.

A result without an eligible optimum still exports its diagnostic evidence, with procurement marked unavailable.

Next: [read your results](results.md), then [bring conductor data](materials.md).
