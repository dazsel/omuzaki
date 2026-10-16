# omuzaki-char: fit to reference

- `fit.reference` fits the body's proportions and shape to reference silhouettes and reports the proposed patch, the IoU before and after and a difference heatmap per view (a PNG in the asset store) without changing anything (#160).
- `fit.apply` writes the same fit into the body part as one undoable edit (#160).
- The silhouettes are inline rows of `#` and `.` for now; reading them from sheet images in the store follows with a PNG decoder.
- The fit solver now keeps the lowest parameter value when two probes cost the same (#2122).
