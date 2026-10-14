# Notices for the CPU rasteriser

The modules `scan`, `geom`, `stroke/` and `hairline` in this folder are **ports** of Skia's software rasteriser, taken through
tiny-skia's Rust port of it: they repeat its algorithms operation for operation, because the rasteriser exists to draw what
the 2D canvas draws. They are not original designs. The Skia and tiny-skia notice, which these files must carry, is below
(BSD-3-Clause), and so is the notice for the accumulation method `scan.rs` also ports from font-rs (Apache-2.0).
The licence register (`docs/licensing/register.md`) has the rows.

| What | Port of | Licence | Copyright |
|---|---|---|---|
| `scan.rs` (edge walking and snapping) | Skia `SkAnalyticEdge`, `SkEdge`, `SkScan_AAAPath`; tiny-skia `edge.rs` | BSD-3-Clause | 2006 The Android Open Source Project; 2011 Google Inc.; 2020 Yevhenii Reizner |
| `scan.rs` (`add_row`, the signed-area accumulation and its prefix sum) | font-rs `draw_line` and `accumulate` | Apache-2.0 | Copyright 2015 Google Inc. (font-rs, https://github.com/raphlinus/font-rs, by Raph Levien) |
| `geom.rs` (chopping, conics) | Skia `SkGeometry`; tiny-skia `path_geometry.rs` | BSD-3-Clause | the same as the first row |
| `stroke/` (the stroker: `mod.rs`, `segments.rs`, `curves.rs`, `joins.rs`, `caps.rs`) | Skia `SkStroke.cpp`; tiny-skia-path `stroker.rs` | BSD-3-Clause | 2008 The Android Open Source Project; 2020 Yevhenii Reizner |
| `hairline.rs` (thin strokes) | Skia `SkScan_Antihair.cpp`, `SkScan_Hairline.cpp`; tiny-skia `scan/hairline*.rs` | BSD-3-Clause | 2006, 2011 The Android Open Source Project; 2020 Yevhenii Reizner |

tiny-skia itself is a dev-dependency of `omuzaki-face` only: the stroker's tests compare its output with tiny-skia's.
`libm` (MIT OR Apache-2.0) supplies the cosine, arc cosine and power the stroker's cubic solver needs, so the output is
bit-identical on every platform.

## BSD-3-Clause

Copyright (c) 2011 Google Inc. All rights reserved.
Copyright (c) 2020 Yevhenii Reizner All rights reserved.

Redistribution and use in source and binary forms, with or without modification, are permitted provided that the
following conditions are met:

  * Redistributions of source code must retain the above copyright notice, this list of conditions and the following
    disclaimer.

  * Redistributions in binary form must reproduce the above copyright notice, this list of conditions and the following
    disclaimer in the documentation and/or other materials provided with the distribution.

  * Neither the name of the copyright holder nor the names of its contributors may be used to endorse or promote
    products derived from this software without specific prior written permission.

THIS SOFTWARE IS PROVIDED BY THE COPYRIGHT HOLDERS AND CONTRIBUTORS "AS IS" AND ANY EXPRESS OR IMPLIED WARRANTIES,
INCLUDING, BUT NOT LIMITED TO, THE IMPLIED WARRANTIES OF MERCHANTABILITY AND FITNESS FOR A PARTICULAR PURPOSE ARE
DISCLAIMED. IN NO EVENT SHALL THE COPYRIGHT OWNER OR CONTRIBUTORS BE LIABLE FOR ANY DIRECT, INDIRECT, INCIDENTAL,
SPECIAL, EXEMPLARY, OR CONSEQUENTIAL DAMAGES (INCLUDING, BUT NOT LIMITED TO, PROCUREMENT OF SUBSTITUTE GOODS OR
SERVICES; LOSS OF USE, DATA, OR PROFITS; OR BUSINESS INTERRUPTION) HOWEVER CAUSED AND ON ANY THEORY OF LIABILITY,
WHETHER IN CONTRACT, STRICT LIABILITY, OR TORT (INCLUDING NEGLIGENCE OR OTHERWISE) ARISING IN ANY WAY OUT OF THE USE OF
THIS SOFTWARE, EVEN IF ADVISED OF THE POSSIBILITY OF SUCH DAMAGE.

## font-rs (Apache-2.0)

`scan.rs` ports the accumulation-buffer method of font-rs, https://github.com/raphlinus/font-rs, by Raph Levien, whose `src/raster.rs` begins "Copyright 2015
Google Inc. All rights reserved." and is licensed under the Apache License, Version 2.0 (https://www.apache.org/licenses/LICENSE-2.0). The method: each edge adds the signed
area of its trapezoid, row by row, to a buffer of deltas, and a running sum along the row turns the deltas into coverage.
**This is a modified port** (Apache-2.0 §4(b)): the code in `scan.rs` is changed from font-rs by clipping to the raster, Skia's edge snapping, and the non-zero rule; it is not the font-rs code verbatim.

Copyright 2015 Google Inc. (font-rs).

Licensed under the Apache License, Version 2.0 (the "License"); you may not use this file except in compliance with the
License. You may obtain a copy of the License at http://www.apache.org/licenses/LICENSE-2.0. Unless required by applicable
law or agreed to in writing, software distributed under the License is distributed on an "AS IS" BASIS, WITHOUT WARRANTIES
OR CONDITIONS OF ANY KIND, either express or implied. See the License for the specific language governing permissions and
limitations under the License.
