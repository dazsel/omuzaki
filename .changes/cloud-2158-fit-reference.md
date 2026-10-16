# omuzaki-char: fit to reference

- `fit.reference` fits the body's proportions and shape to reference silhouettes and reports the proposed patch, the IoU before and after and a difference heatmap per view (an inline base64 PNG) without changing anything (#160).
- `fit.apply` writes the same fit into the body part as one undoable step, and is refused if the body changed while the fit ran (#160).
- Both are jobs: the fit runs off the document lock, reports progress and stops on a cancel; a dry run checks the arguments and fits nothing.
- The fit starts from the body as stored, so the IoU before is the character's own and applying never makes its outline cost more.
- The silhouettes are inline rows of `#` and `.` for now; reading them from sheet images in the store follows with a PNG decoder.
- The fit solver now keeps the lowest parameter value when two probes cost the same (#2122).
- The fit is more robust: it aligns the body to a reference by its mean column, reads each pixel's coverage so the cost changes smoothly, ends each line search with a parabolic step and skips axes the views cannot see. It recovers the test character on all four templates, with and without the mitten hands.
