# omuzaki-char: silhouette fit solver (CB-03)

- `fit::solver::fit` fits the body's 8 proportions and 5 shape axes to up to three binary silhouette masks (front, side, back).
- Deterministic coordinate descent with golden-section line searches in a fixed parameter order, at most 200 evaluations, no GPU and no threads.
- Sole and crown are aligned, so absolute size drops out; the cost is IoU plus a boundary-distance term, and isolated mask specks are removed first.
- Other built parts stay fixed and are composited into the silhouette; `FitResult` reports the parameters, IoU before and after, per view.
