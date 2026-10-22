# Offline renders reach 16 samples on any GPU

- `QualityMode::Offline` now gets the samples it asks for when the GPU has fewer (#653): the geometry passes draw in
  rounds, each moved by a fixed sub-pixel offset, and the rounds are averaged in half float before bloom, depth of field
  and `Resolve`. On a 4x GPU the offline preset draws four rounds, on an 8x GPU two.
- The offsets come from a fixed table, so an offline frame is still exact: two runs are byte-identical and the frame
  index never moves a sample. Together with the GPU's standard pattern they make a rook pattern of 16 samples, so a
  one-pixel line shows 16 coverage levels where hardware 4x shows 5.
- Shadows and texture painting run once per frame. The frame report counts every round: the geometry passes' times and
  draw calls add up across rounds, and `Resolve` includes the averaging.
- Live and high quality, and offline renders that ask for no more samples than the hardware has, draw exactly as before.
