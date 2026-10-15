# omuzaki-char: horns, headphones, hats, glasses, tails and wings

- New part kinds `horns`, `headphones`, `hat`, `glasses`, `tail` and `wings` (CB-43): horns in four curl presets with an ember-crack glow, headphones with optional cat ears and a mic, five hat styles, four glasses styles, four tails and four wings (#132, #344, #332, #196).
- Horns, headphones, hats and glasses are built on the head's published surface and the ear anchors, skinned rigidly to the head; `attach` moves one as a piece.
- A tail is a spring-bone chain (`SpringChain::tapered`) rooted at the tailbone; a 90 degree hips turn makes the fox tail lag about 108 ms and settle.
- Wings have a three-bone frame per wing folded through a `customNN` channel (`fold_channel`, 0 folded, 1 spread) and a spring flutter at the tip; they stay clear of the back folded and spread.
- Every kind is deterministic and builds closed shells on all four templates; recolouring a palette slot leaves the geometry alone.
