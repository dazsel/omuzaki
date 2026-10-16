# omuzaki-char: fit to reference

- `fit.reference` fits the body's proportions and shape to reference silhouettes and reports the proposed patch, the IoU and cost before and after, and a difference heatmap per view (an inline base64 PNG) without changing anything (#160).
- `fit.apply` writes the same fit into the body part as one undoable step, and is refused if the body changed while the fit ran (#160).
- A view's silhouette is text rows, a base64 PNG, or a PNG in the asset store by its content hash (#160).
- The character's other parts (head, hair, accessories, garments) are drawn into the silhouette with every candidate body, as built on the stored body, so a reference of the whole character fits the body inside it; `parts` picks which (#159, #160).
- Both are jobs: the fit runs off the document lock, reports progress and stops on a cancel; a dry run checks the arguments and fits nothing.
- The fit starts from the character as stored, so the IoU before is its own and applying never makes its outline cost more.
- The fit solver keeps the lowest parameter value when two probes cost the same (#2122), aligns the body to a reference by its mean column, reads each pixel's coverage so the cost changes smoothly, and ends each line search with a parabolic step.
