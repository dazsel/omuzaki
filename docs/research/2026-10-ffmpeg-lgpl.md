---
title: "LGPL FFmpeg as shared libraries: build proof (2026-10-15)"
summary: "An LGPL FFmpeg n9.0.2 built as shared libraries encodes ProRes 4444 and VP9 alpha through ffmpeg-next; what the release must ship and what the owner run still has to show"
area: research
---

# LGPL FFmpeg as shared libraries: build proof (2026-10-15)

**Part of #1424.** The choice (FFmpeg, not GStreamer, linked dynamically through `ffmpeg-sys-next`) is made in ADR 0039 and
`docs/design/record.md` D1; the conditions are in `docs/research/2026-10-licence-audit.md` (B4, blocker 7). This note proves
the build on Linux. The spike is `spikes/ffmpeg-lgpl` (its own workspace). The clips and the FFmpeg tree stay outside the
repository (`docs/decisions/0003-assets-outside-git.md`). `$PREFIX` below is the scratch install folder.

## 1. Build recipe

| What | Version | Source |
|---|---|---|
| FFmpeg | n9.0.2 (commit `946fcce07b6d`) | git tag `n9.0.2` of the FFmpeg repository on GitHub |
| nv-codec-headers (MIT) | n13.1.15.0 (commit `0a6fba9a2820`) | git tag on GitHub; `make install PREFIX=$PREFIX` |
| libvpx (BSD-3) | 1.14.0, Ubuntu 24.04 `libvpx-dev` 1.14.0-1ubuntu2.3 | apt, linked as `libvpx.so.9` |
| gcc / nasm | 13.3.0 / 2.16.01 | Ubuntu 24.04 |

The tarball on ffmpeg.org could not be used: the environment's network policy refuses `ffmpeg.org` and `git.ffmpeg.org` (HTTP
403 on CONNECT). The git tag from GitHub is the same source; the two were not compared byte for byte.

```sh
export PKG_CONFIG_PATH=$PREFIX/lib/pkgconfig
./configure --prefix=$PREFIX --enable-shared --disable-static \
            --enable-libvpx --enable-ffnvcodec --disable-doc --disable-debug
make -j4 && make install
```

`--enable-gpl`, `--enable-nonfree` and `--enable-version3` are not passed. The spike builds with
`PKG_CONFIG_PATH=$PREFIX/lib/pkgconfig LD_LIBRARY_PATH=$PREFIX/lib cargo build --release --locked`.

## 2. No GPL, no nonfree

`$PREFIX/bin/ffmpeg -version` (first lines) and `-buildconf`:

```
ffmpeg version n9.0.2 Copyright (c) 2000-2026 the FFmpeg developers
built with gcc 13 (Ubuntu 13.3.0-6ubuntu2~24.04.1)
configuration: --prefix=$PREFIX --enable-shared --disable-static --enable-libvpx --enable-ffnvcodec --disable-doc --disable-debug
libavutil      61.  1.102 / 61.  1.102
libavcodec     63.  1.102 / 63.  1.102
libavformat    63.  1.102 / 63.  1.102
libavdevice    63.  1.102 / 63.  1.102
libavfilter    12.  1.102 / 12.  1.102
libswscale     10.  1.102 / 10.  1.102
libswresample   7.  1.102 /  7.  1.102
```

- `configure` prints `License: LGPL version 2.1 or later`.
- `config.h`: `CONFIG_GPL 0`, `CONFIG_NONFREE 0`, `CONFIG_VERSION3 0`, `CONFIG_GPLV3 0`.
- `ffmpeg -buildconf` contains no `gpl`, `nonfree` or `version3` (0 matches).
- No x264, x265 or fdk-aac symbol is exported by `libavcodec.so` (0 matches in `nm -D`).
- `libavcodec.so` links only libvpx, liblzma, zlib and libc-level libraries besides the other libav libraries.

## 3. The spike links dynamically

`ffmpeg-next 9.0.0` and `ffmpeg-sys-next 9.0.1` with `default-features = false` and only `format` and `software-scaling`:
no `static`, no `build`. `ldd` on the release binary:

```
libavutil.so.61 => $PREFIX/lib/libavutil.so.61
libavformat.so.63 => $PREFIX/lib/libavformat.so.63
libswscale.so.10 => $PREFIX/lib/libswscale.so.10
libavcodec.so.63 => $PREFIX/lib/libavcodec.so.63
libswresample.so.7 => $PREFIX/lib/libswresample.so.7
libvpx.so.9 => /lib/x86_64-linux-gnu/libvpx.so.9
```

## 4. The clips

`ffmpeg-lgpl-encode <dir>`: seed 1424, 1920x1080 RGBA, 90 frames at 30 fps (a drifting gradient inside a disc with a soft
alpha edge; the alpha plane holds 0, 255 and 221,338 intermediate values of 2,073,600 pixels on frame 45). Two runs gave
byte-identical file sizes.

| File | ffprobe |
|---|---|
| `prores4444.mov` (`prores_ks`, profile 4, `alpha_bits` 16) | `codec_name=prores`, `profile=4444`, `pix_fmt=yuva444p12le`, 1920x1080, `r_frame_rate=30/1`, `nb_read_frames=90`; 90,998,572 bytes |
| `vp9-alpha.webm` (`libvpx-vp9`, `auto-alt-ref=0`, crf 30, 1 s keyframes) | `codec_name=vp9`, `ALPHA_MODE=1` tag, `nb_read_frames=90`; 260,607 bytes. With the `libvpx-vp9` decoder: `pix_fmt=yuva420p` |

Notes:
- ffprobe reports ProRes 4444 as 12-bit because that is what the decoder outputs; the spike feeds 10-bit `yuva444p10le`.
- With FFmpeg's built-in VP9 decoder ffprobe says `yuv420p`: the alpha plane travels as BlockAdditional data that only the
  `libvpx-vp9` decoder reads. Decoding with it and extracting the alpha plane gave 0 to 255 values, so the alpha is real. Readers of our
  WebM files that do not use libvpx will see opaque video; D3 and D9 of `record.md` already assume this.
- `libvpx-vp9` needs `auto-alt-ref=0` for alpha, and the muxer needs the `alpha_mode=1` stream tag; both are in the spike.

## 5. Encode timing

Machine: a cloud VM, software encoders only. Release build. Time is
`send_frame` plus draining packets; the RGBA to YUVA conversion (swscale) is counted apart. Per-frame lines are printed by the
binary.

| Encoder | mean | p50 | p95 | max | convert (mean) |
|---|---|---|---|---|---|
| `prores_ks` 4444 | 212.6 ms | 209.1 ms | 240.1 ms | 245.6 ms | 8.5 ms |
| `libvpx-vp9` realtime, `cpu-used=5`, `row-mt` | 50.2 ms | 33.4 ms | 98.1 ms | 924.2 ms | 7.0 ms |

Both are over the 33.3 ms frame period of 30 fps on this VM. ProRes by `prores_ks` is about 6x too slow for real time
here (it is a software encoder with no `slices` or thread tuning in this spike), and the VP9 median is inside the period but the tail is not.
The VP9 maximum (about 0.9 s in both runs) is the first frame (856 ms in the printed run; frames 1 and 2 take 1.4 ms because libvpx buffers
ahead, so the cost lands later in the stream), so a record worker should warm the encoder before the first real frame. These are one machine's numbers; the measurement run (D12)
measures `Dev`.

## 6. The LGPL checklist, and what a release ships

**The ffmpeg.org legal page was not reachable** (`ffmpeg.org` is refused by this environment's network policy), so the checklist
below is **not quoted from the source**. It is the audit's summary of https://ffmpeg.org/legal.html, as written on 2026-10-11
in `docs/research/2026-10-licence-audit.md` (the FFmpeg rows), and the point-by-point quotation with a fetch date is open (§7).
What was read first-hand today (2026-10-15) is `LICENSE.md` at tag `n9.0.2`
(https://github.com/FFmpeg/FFmpeg/blob/n9.0.2/LICENSE.md): "Most files in FFmpeg are under the GNU Lesser General Public License
version 2.1 or later" and the GPL parts "are not used by default, you have to explicitly pass `--enable-gpl` to configure to
activate them. In this case, FFmpeg's license changes to GPL v2+."

From the audit (not first-hand): no `--enable-gpl` or `--enable-nonfree`; link dynamically; ship the exactly matching source plus build notes and
diffs on the same server; credit LGPLv2.1 in the About box and EULA; no reverse-engineering ban; do not rename the DLLs. LGPL §6 adds a
source offer valid at least 3 years.

| Condition | What our release does |
|---|---|
| No GPL or nonfree | The build in §1; the pinned configure line is recorded and `probe()` refuses `libx264`, `libx265`, `libfdk_aac` (D1). A CI check that greps `-buildconf` is cheap and not done here. |
| Dynamic linking, unrenamed libraries | Ship the `libav*.so`, `.dll` and `.dylib` files under their upstream names, next to the app, not merged into the executable; never the `static` or `build` features (the spike's manifest says so). |
| Source offer | Host the exact tag (n9.0.2), our configure line, any patch (none today) and the nv-codec-headers and libvpx versions on our own server, linked from the About box; keep them for at least 3 years after the last release that used them. |
| Build notes | §1 of this note, kept with the source. |
| About-box credit | "This software uses libraries from the FFmpeg project under the LGPLv2.1", with the source link. |
| EULA | No term that forbids reverse engineering for debugging a modified library. |

libvpx is BSD-3 (keep its notice); `prores_ks` is part of libavcodec, but Apple has not licensed it, and patents are a separate legal-review item
(audit row "H.264, HEVC and AAC patent licensing").

## 7. Left for the owner run, and for the maintainers

- **Hardware encoders:** `h264_nvenc`, `hevc_nvenc` and `av1_nvenc` are compiled in (`ffmpeg -encoders`) but cannot be opened here (no GPU).
  Still to run on real hardware: NVENC, AMF, QSV (libvpl), VideoToolbox (`prores_videotoolbox`, `h264_videotoolbox`), each opened at 1080p
  with a clip. AMF and libvpl headers were not installed here.
- **Windows:** build with MSVC or MSYS2 (same configure flags), `dumpbin /dependents` on the spike binary to show the DLLs, and check
  the DLL names are unrenamed. This needs the `spike-build` workflow (#1941).
- **macOS:** the same `otool -L` check.
- **The legal page:** fetch https://ffmpeg.org/legal.html from a machine that can reach it, and replace the §6 summary with the quoted checklist and the fetch date.
- **`ffmpeg-next` and `ffmpeg-sys-next` are WTFPL.** `cargo deny check licenses` with the repository's `deny.toml` rejects both for the spike's
  lockfile (as the audit predicted). The spike is outside the workspace, so no CI run sees it; when the `ffmpeg` feature of
  `omuzaki-stream` lands, `deny.toml` needs a decision (an exception, or the `bindgen` layer from D1). Not changed here.
