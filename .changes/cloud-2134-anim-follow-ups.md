# omuzaki-anim: animation follow-ups (exact Neck·Head split, hips scale, life tuning, range provenance)

- `BusAnimator` splits the head rotation by slerp, so Neck · Head is exactly the bus rotation (#1875).
- `omuzaki_anim::poses::retarget_hips` / `scale_hips` scale a pose's hips offset by `y_dst / y_src` between characters of different height (#77).
- `LifeParams` gains the life layer's remaining tuning numbers (a `laugh` group, and more fields on blink, gaze, head, face and springs); defaults reproduce today's output, and older files still load (#78).
- A calibration range records whether it is a default or a fitted one (`RangeSource`, `Calibration::set_fitted`), and the retarget reads it instead of guessing from the span (#84).
- A parsed `PoseClip`'s `length`, `looped` and `easing` are read through accessors, so a parsed clip stays valid (#2099).
