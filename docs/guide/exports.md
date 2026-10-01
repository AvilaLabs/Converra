# Export and review a design

A review package collects the calculation and the inputs needed to interpret and rerun it. Use **Reports → Review package** after a completed study.

The package includes exact case bytes, the record, a readable HTML report and decision summary, all resolved material bundles, and an artifact manifest. Eligible selections also include modeled procurement outputs. Browser exports a tar archive; extract it before using the CLI verifier.

## From the CLI

```bash
optcoil review-package runs/design.json case.json \
  --output runs/design-review
optcoil verify-package runs/design-review
```

Choose a new destination. Export protects existing evidence from overwrite. The package README lists its rerun command, including repeated `--dataset-bundle` flags where needed; run that command from the extracted package directory.

## When opening a saved record

A record does not contain every original input byte. Use **Attach original case** in Reports; the case's byte hash must match the record before a rerunnable package can be exported. Load matching external material dependencies too.

Keep a saved study workspace when you want named variants and historical comparisons as well as one run's review package.

## What verification checks

Package verification checks artifact bindings, identities and modeled arithmetic. Acceptance checks separately recompute the selected design and baseline under the declared screening model. An engineer still needs to review material applicability and the structural, thermal, quench and manufacturing evidence relevant to the decision.

For a study with no eligible optimum, the diagnostic result remains reviewable. Its missing procurement output is an explicit consequence of that decision.
