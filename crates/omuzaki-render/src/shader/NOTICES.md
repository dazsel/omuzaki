# Notices for the shaders

- `post_resolve.wgsl`, `aces`: the ACES filmic curve as three.js implements it (`ACESFilmicToneMapping` in
  `src/renderers/shaders/ShaderChunk/tonemapping_pars_fragment.glsl.js`, three@0.170.0): the input and output matrices, the
  fit coefficients and the `exposure / 0.6` pre-scale are three.js's, from Stephen Hill's fit. A copied constant table,
  so the three.js notice below applies. The Hill fit is distributed under the MIT licence by its author (BakingLab
  `ACES.hlsl`; unverified against the primary source, tracked in the register row).
- `post_bloom.wgsl`, `toon.wgsl`: written against the behaviour of three.js's `UnrealBloomPass` and light units, not
  from its source; `tests/golden/three_bloom_*.png` are three.js renders used as references. Nothing to carry.

MIT License

Copyright © 2010-2024 three.js authors

Permission is hereby granted, free of charge, to any person obtaining a copy of this software and associated documentation files (the "Software"), to deal in the Software without restriction, including without limitation the rights to use, copy, modify, merge, publish, distribute, sublicense, and/or sell copies of the Software, and to permit persons to whom the Software is furnished to do so, subject to the following conditions:

The above copyright notice and this permission notice shall be included in all copies or substantial portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE SOFTWARE.
