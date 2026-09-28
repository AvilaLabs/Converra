# Reserved OC-003 challenge region

These are attributed **CC BY 4.0** adaptations of the same third-party dataset described in [the parent attribution notice](../README.md), with nominal temperatures 20–50 K. Source files remain in `../source/`.

The entire 45 K plane is excluded from training in the reserved challenge. The benchmark and region were declared before evaluating the logarithmic candidate; these points were absent from the original 20–40 K development dataset. Only the other two axes' strictly interior points are scored under the predeclared policy. This remains a test on the same measured specimen, not an independent batch.

`tools/prepare_oc003_challenge.py` reuses the unchanged original preparation code. The preparation-source hash covers the original script bytes followed by the challenge script bytes. Transformations and limitations otherwise follow the parent notice.
