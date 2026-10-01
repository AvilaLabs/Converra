# Costs, grading and procurement

The cost model combines installed conductor length, purchasing/scrap rules, assembly and declared joint or splice costs. Prices belong to the case; results retain whether they are synthetic, estimated, published or quoted.

## Price the winding you actually declared

For a simple scrap model, purchased length is installed length multiplied by `1 + scrap_fraction`. Each grade uses its own price per metre. Joint and assembly terms remain in the total even when a candidate uses less conductor.

With a piece policy, the ledger instead buys declared piece offerings and accounts for the resulting splices and remnants. Module boundaries and per-strand purchasing change those counts. Specify them explicitly before interpreting the cheapest offering.

## Use grading deliberately

Assign conductor specifications to declared winding regions or permitted interfaces. Each specification retains its own dataset, price and material assumptions. Supply all external dependencies before running a graded case.

A cheaper grade can save material cost while increasing tape count, purchased length or transition costs. Inspect the regional allocation and complete ledger, then compare the same requirement and sampling plan.

## Read the procurement outputs

The BOM contains installed and purchased metres per specification, turn ranges, geometry, joints/pancakes and reconciled ledger totals. Piece policies add offering, splice and remnant quantities.

A record without a passing eligible optimum cannot produce a purchasing design. Review export preserves that result with procurement unavailable. A modeled BOM or RFQ summary describes the declared design; supplier availability, lead times, tooling, insulation and splice qualification need their own inputs.

References: [grading](https://github.com/AvilaLabs/Converra/blob/main/docs/GRADING.md) and [BOM/piece policy](https://github.com/AvilaLabs/Converra/blob/main/docs/BOM.md).
