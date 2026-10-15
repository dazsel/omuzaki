---
title: "Mascot window: transparent, always-on-top, click-through with wgpu"
summary: "transparent, always-on-top, click-through with wgpu"
area: research
---

# Mascot window: transparent, always-on-top, click-through with wgpu

Status: research, partial. Part of BR-25. Read against the main branch as of 2026-10-13.
The spike binaries and the cost measurement on `Dev` are not in this page; section 9 is the exact run-list.

## 1. Summary

| Feature | Windows | macOS | Linux X11 | Linux Wayland |
|---|---|---|---|---|
| Transparent surface (wgpu) | DX12 default (`DxgiFromHwnd`): **Opaque only**. DX12 `DxgiFromVisual` (wgpu makes the visual from the HWND): alpha modes offered; end-to-end appearance not verified. Vulkan: driver-defined (not verified). | Metal: Opaque + PostMultiplied (verified). | Vulkan / GL: not verified. A 32-bit TrueColor visual is chosen by winit. | Not verified . |
| Always on top | `WS_EX_TOPMOST` in winit (verified). | `kCGFloatingWindowLevel` in winit (verified). | `_NET_WM_STATE_ABOVE` in winit (verified). | **No-op** in winit (verified). Needs wlr-layer-shell, outside winit. |
| Hide from taskbar | `with_skip_taskbar` in winit (verified). | Not in winit; only app-wide `ActivationPolicy::Accessory`. | Not in winit (verified). | Not in winit. |
| Hide from Alt-Tab | Not in winit; `WS_EX_TOOLWINDOW` needs `windows-sys` (not verified on a machine). | Not applicable (Dock / Cmd-Tab, not verified). | Not in winit. | Not in winit. |
| Opacity | No winit API. Shader-side multiplication is the portable path. | No winit API. | No winit API. | No winit API. |
| Whole-window click-through | `WS_EX_TRANSPARENT \| WS_EX_LAYERED` in winit (verified). | `setIgnoresMouseEvents` in winit (verified). | Empty XShape input rectangles in winit (verified). | Empty `wl_surface` input region in winit (verified). |
| Per-pixel click-through | Not in winit; needs polling or custom hit testing. | Not in winit; needs polling. | Not in winit; needs XShape rectangles from the alpha mask (x11rb). | Not in winit; needs a `wl_region` built from the alpha mask, passed to `wl_surface.set_input_region` (not verified, cloud). |

The "verified" entries above are facts about winit 0.30.13 and wgpu-hal 30.0.1 source. Whether the OS
honours them on a real desktop is marked "not verified" and is the job of the run-list in section 9.

**Provisional recommendation (to be confirmed on machines):** keep eframe for the main window; build the mascot as its
own winit window with its own wgpu surface, not as an egui secondary viewport (section 6). Use whole-window
click-through first, and treat per-pixel click-through as a follow-up that polls the cursor against the alpha mask.

## 2. Method and legend

- **Versions pinned** (`Cargo.lock` on main): winit 0.30.13, wgpu 30.0.1, wgpu-hal 30.0.1, wgpu-types 30.0.1,
  eframe / egui / egui-winit / egui-wgpu 0.36.2. Sources read from the local cargo registry
  (`~/.cargo/registry/src/*/`) after `cargo fetch`. Paths below are relative to those crate roots.
- **Line anchors** were re-checked against those sources. Where the earlier scoping note cited a line that had moved,
  the anchor here is the corrected one.
- **Legend**
  - *verified in source*: read in the pinned crate source at the cited line.
  - *documented*: stated in a public doc page we could open. None are used on this page yet.
  - *not verified*: needs a network page that returned 403 in the research environment (docs.rs, learn.microsoft.com,
    Mesa's GitLab, other repositories' issue trackers), or needs a real OS, GPU or compositor. These go to section 9.
- **Reachability from the research environment:** raw.githubusercontent.com and crates.io work; docs.rs, learn.microsoft.com,
  gitlab.freedesktop.org, wayland.app and other repositories' GitHub pages return 403.

## 3. Transparency: `CompositeAlphaMode` per backend

### 3.1 What wgpu reports

`wgpu-types` defines the modes. `Auto` is documented as choosing `Opaque` or `Inherit` (verified:
`wgpu-types-30.0.1/src/surface.rs:114-117`). `PreMultiplied` is at `:126`, `PostMultiplied` at `:132`.

Each backend fills `SurfaceCapabilities::alpha_modes`:

| Backend | Target | Offered (verified in source) | Source |
|---|---|---|---|
| DX12 | plain HWND (`SurfaceTarget::WndHandle`) | `Opaque` only | `wgpu-hal-30.0.1/src/dx12/adapter.rs:1364-1365` |
| DX12 | `Visual`, `VisualFromWndHandle`, `SurfaceHandle`, `SwapChainPanel` | `Auto`, `Inherit`, `Opaque`, `PostMultiplied`, `PreMultiplied` | `dx12/adapter.rs:1366-1375` |
| DX12 | mapping to DXGI | `PreMultiplied` to `DXGI_ALPHA_MODE_PREMULTIPLIED`; `PostMultiplied` to `DXGI_ALPHA_MODE_STRAIGHT`; `Opaque` to `IGNORE` | `auxil/dxgi/conv.rs:293-297` |
| Vulkan | driver's `supportedCompositeAlpha` | whatever the driver reports, via `map_vk_composite_alpha` | `vulkan/swapchain/native.rs:178`, `vulkan/conv.rs:542-557` |
| Metal | `CAMetalLayer` | `Opaque`, `PostMultiplied` | `metal/adapter.rs:468-471` |
| GLES | | `Opaque` only, marked `//TODO` | `gles/adapter.rs:1376` |

Two notes:

- **Vulkan is driver-defined.** Nothing in wgpu forces a transparent mode, so the answer for NVIDIA, AMD, Intel and
  Mesa on Windows and Linux is *not verified*. This is the first thing the `caps` bin in section 9 measures.
- **Metal's `PostMultiplied` is a layer flag.** `metal/surface.rs:268-271` sets `setOpaque(false)` on the
  `CAMetalLayer` for `PostMultiplied` and `setOpaque(true)` for `Opaque`. The surface is still presented with
  premultiplied colour unless the shader says otherwise; see section 7.

### 3.2 DX12 needs the `DxgiFromVisual` swapchain kind for alpha

The DX12 presentation system is a backend option, `Dx12SwapchainKind` (verified: `wgpu-types-30.0.1/src/backend.rs:675-690`):

- `DxgiFromHwnd` (the default): a DXGI swapchain made from the HWND. Its docs say it does not support transparency.
  This is the `WndHandle` case that offers `Opaque` only (section 3.1).
- `DxgiFromVisual`: wgpu creates a DirectComposition visual over the window from the HWND
  (verified: `wgpu-hal-30.0.1/src/dx12/instance.rs:142-147`, which maps it to `SurfaceTarget::VisualFromWndHandle`).
  That target offers the full set of alpha modes (section 3.1). Its docs say it supports transparent windows, at the
  cost of RenderDoc support.

eframe does not hard-code the kind. egui-wgpu passes `backend_options` through to wgpu when it creates the
instance (verified: `egui-wgpu-0.36.2/src/setup.rs:124`, `:289`), so a Windows build can select `DxgiFromVisual`
through eframe's wgpu setup. This removes the need for our own unsafe visual in the common case.

The raw hook `SurfaceTargetUnsafe::CompositionVisual(*mut c_void)` (verified: `wgpu-30.0.1/src/api/surface.rs:437-443`)
is only needed if we manage the DirectComposition tree ourselves. Whether `DxgiFromVisual` gives correct per-pixel
alpha on a real desktop is *not verified*; it is the first Windows item in the run-list.

### 3.3 egui-wgpu chooses the mode for eframe

`egui-wgpu-0.36.2/src/winit.rs:272-290`: when transparency is requested, the painter prefers `PreMultiplied`, then
`PostMultiplied`, and otherwise logs a warning and uses `Auto`. The comment there reads "Prefer pre multiplied over
post multiplied!". On Metal only `PostMultiplied` is offered (3.1), so on macOS the actual choice is
`PostMultiplied`, and whether the output is correctly encoded has to be checked on a Mac (section 7).

## 4. Window attributes per OS

### 4.1 Always on top (window level)

winit's `set_window_level` is documented as "just a hint to the OS, and the system could ignore it"
(verified: `winit-0.30.13/src/window.rs:1179-1183`).

| OS | Mechanism | Source |
|---|---|---|
| Windows | `WS_EX_TOPMOST` in the style when `ALWAYS_ON_TOP`; `SetWindowPos(HWND_TOPMOST)` on change | `src/platform_impl/windows/window_state.rs:277`, `:346` |
| X11 | `_NET_WM_STATE_ABOVE` toggled | `src/platform_impl/linux/x11/window.rs:1073-1074` |
| macOS | `NSWindow.level` = `kCGFloatingWindowLevel` | `src/platform_impl/macos/window_delegate.rs:1531-1533` |
| Wayland | `set_window_level` is an empty function | `src/platform_impl/linux/wayland/window/mod.rs:430` |

The public docs for `WindowLevel` mark Wayland as unsupported (verified: `src/window.rs:1798-1803`). So on Wayland
the mascot cannot be always-on-top through winit. wlr-layer-shell (in `wayland-protocols-wlr` in the registry)
can place a surface above normal windows, but it is not in winit. Which compositors support it (GNOME is reported not
to) is *not verified*.

### 4.2 Transparent window

- **X11:** when `transparent` is set and no visual is given, winit picks a 32-bit `TrueColor` visual
  (verified: `x11/window.rs:241-246`). Whether the compositor then blends it is *not verified*: it needs a
  running compositor.
- **Windows:** unless `no_redirection_bitmap` is set, winit calls `DwmEnableBlurBehindWindow` with an empty blur
  region (verified: `platform_impl/windows/window.rs:1230-1243`; flag at `platform/windows.rs:489`, `:560-561`).
  This is winit's window setup, which eframe uses for `transparent: true`. It is separate from the wgpu swapchain:
  on DX12 the swapchain kind (section 3.2) decides whether the surface can carry alpha.
- **macOS:** winit's `set_transparent` doc says it resets the window's background colour (verified in doc:
  `src/window.rs:936-946`). The macOS transparency path itself was not read in this pass: *not verified*.
- **Wayland:** not verified.

### 4.3 Taskbar and Alt-Tab

- **Windows:** `with_skip_taskbar` (`platform/windows.rs:500`, `:572-573`) sets the window's `ON_TASKBAR` flag off;
  winit adds `WS_EX_APPWINDOW` only when that flag is on (`platform_impl/windows/window_state.rs:273-275`). Changing
  it after creation calls `ITaskbarList` through COM (`platform_impl/windows/window.rs:1498`). Alt-Tab is governed by `WS_EX_TOOLWINDOW`, which winit does not set. Adding it
  needs `windows-sys` and a machine to confirm the Alt-Tab behaviour: *not verified*.
- **macOS:** winit has no skip-taskbar call. The only switch is app-wide `ActivationPolicy::Accessory` (verified:
  `platform/macos.rs:257-268`, applied at `platform_impl/macos/event_loop.rs:229`). That hides the Dock icon for the whole
  app, so it also affects the main eframe window. Per-window Dock and Cmd-Tab hiding is *not verified*.
- **X11:** no skip-taskbar in winit (verified: no `skip_taskbar` under `platform_impl/linux/`). The only hint is
  `_NET_WM_WINDOW_TYPE` through `with_x11_window_type` (verified: `platform/x11.rs:165`). Whether a given window
  manager then hides it from the taskbar is *not verified*.
- **Wayland:** no mechanism in winit; not verified.

### 4.4 Opacity

winit has no window-opacity API (verified: no `opacity` setter in `window.rs`, `platform_impl/windows/window.rs`,
`platform_impl/linux/x11/window.rs` or `platform_impl/macos/window_delegate.rs`).

The portable answer is to multiply the premultiplied output by a factor in the shader, which is exact for
premultiplied colour and needs no OS support. Native options (`SetLayeredWindowAttributes` on Windows,
`NSWindow.alphaValue` on macOS, `_NET_WM_WINDOW_OPACITY` on X11) exist, but they are outside winit and their
behaviour is *not verified*.

## 5. Click-through

### 5.1 Whole-window click-through (winit `set_cursor_hittest(false)`)

| OS | Mechanism | Source |
|---|---|---|
| Windows | `WS_EX_TRANSPARENT \| WS_EX_LAYERED` | `platform_impl/windows/window_state.rs:300-301`; `set_cursor_hittest` at `platform_impl/windows/window.rs:632` |
| macOS | `setIgnoresMouseEvents(!hittest)` | `platform_impl/macos/window_delegate.rs:1188` |
| X11 | `shape_rectangles(SET, INPUT, ...)` with an empty list when not hittable, the full rectangle when hittable | `platform_impl/linux/x11/window.rs:1623-1656` |
| Wayland | `wl_surface.set_input_region(None)` when hittable (an unbounded region); an empty `wl_region` when not hittable | `platform_impl/linux/wayland/window/mod.rs:585-598` |

This gives whole-window click-through on all four targets, as a hint from winit's point of view. Whether the OS obeys
it in each case is *not verified*.

### 5.2 Per-pixel click-through

winit has no per-pixel API. Each option below is an extra mechanism outside winit:

- **Windows:** poll `GetCursorPos`, read the mascot's alpha at that pixel from a CPU copy, and toggle
  `set_cursor_hittest`. Or a layered window with a region. *Not verified*: the polling path needs a real
  desktop, and the cursor is read across the whole session.
- **macOS:** poll `NSEvent.mouseLocation` and toggle `setIgnoresMouseEvents`. *Not verified*.
- **X11:** set the input shape from the alpha mask as XShape rectangles, through `x11rb` (already in the registry at
  0.13). This is the most direct route; whether the server honours it per frame is *not verified*.
- **Wayland:** build a `wl_region` from the alpha mask and pass it to `wl_surface.set_input_region`. winit only uses
  two cases, unbounded (`None`) when hittable and empty when not (verified: `platform_impl/linux/wayland/window/mod.rs:585-598`),
  so a mask region is our own code and *not verified*. This is the mask route the design uses on Wayland.

Polling needs the global cursor position. Whether a Wayland client can read it is *not verified*; the
protocol and compositor side were not checked here. The mask route above does not depend on it. This connects to the
OS input-capture work; that page is linked here, not repeated.

## 6. Egui multi-viewport or its own window

egui's `ViewportBuilder` carries `transparent` (`egui-0.36.2/src/viewport.rs:306`), `taskbar` (`:320`),
`window_level` (`:324`) and `mouse_passthrough` (`:325`). egui-winit applies them:

- `transparent` to each new window (`egui-winit-0.36.2/src/lib.rs:2068`);
- `window_level` (`:2077`);
- `taskbar` only under `cfg(target_os = "windows")` (`:2184-2192`);
- `mouse_passthrough` through `set_cursor_hittest(!passthrough)` (`:2236-2238`; runtime command at `:1935-1938`).

The blocker is in eframe. It passes only the **root** viewport's `transparent` to the single painter
(verified: `eframe-0.36.2/src/native/wgpu_integration.rs:228`), and every surface uses that flag
(`egui-wgpu-0.36.2/src/winit.rs:272`). So a transparent secondary viewport needs a transparent root window,
which the main UI does not want.

**Only one winit event loop per process (verified).** winit keeps a process-wide `EVENT_LOOP_CREATED` flag
(`winit-0.30.13/src/event_loop.rs:69`); a second `EventLoopBuilder::build` returns
`EventLoopError::RecreationAttempt` (`:117-119`). So the mascot cannot start a second loop next to eframe's.

**Two routes that avoid a second loop:**

- **Shared loop (supported API, verified in source).** eframe exposes `create_native` (`eframe-0.36.2/src/lib.rs:376`),
  which takes an `EventLoop` we own, and `EframeWinitApplication`, documented as running "on your own `EventLoop`
  … with other windows you manage outside of eframe" (`src/native/run.rs:486-492`). We would create the mascot
  window and its wgpu surface in the same `ApplicationHandler`. eframe's painter and its root-viewport transparency
  would then not apply to the mascot surface. Whether two surfaces on one loop behave correctly is *not verified*; it is a follow-up spike.
- **Separate process.** Its own loop and surface, no shared state with eframe. Heavier to build and to feed frames
  into, but independent of eframe's loop.

Either route keeps the main window opaque. Whether an egui secondary viewport with its own transparent painter would
work is *not verified*; it needs a probe.

## 7. Premultiplied encodings: the shared picture

The test picture for the mascot window is the one from the Spout spike, `spikes/broadcast-out/spout-out/src/scene.wgsl`
(premultiplied colour, soft edges, every alpha level at the borders, known-value patches). Reusing it means the
fringe check matches the Spout findings.

The Spout spike found that **the alpha encoding is not one thing across outputs**: Spout has no alpha-mode flag,
OBS's Spout2 source ignores alpha until the user picks a composite mode, NDI wants straight alpha, and a virtual
camera has no alpha at all (`spikes/broadcast-out/README.md`, "Alpha, and what OBS does with it", lines 135-154).

For the window, the same care applies per platform:

- **Windows DX12** `PreMultiplied` maps to `DXGI_ALPHA_MODE_PREMULTIPLIED` (section 3.1), so the shader output
  should be premultiplied. Verified in source; the visible fringe is *not verified*.
- **macOS Metal** `PostMultiplied` sets `setOpaque(false)`, and egui-wgpu prefers `PreMultiplied` (section 3.3).
  If the layer expects straight colour, the edges will show a dark or bright halo over white or black. This is
  the check most likely to fail, and it needs a Mac.
- **Vulkan** modes depend on the driver (section 3.1); *not verified*.

The fringe check is: render the scene over a white and a black background, compare the edge pixels to the expected
premultiplied result, and record the colour of the known patches.

## 8. Provisional recommendation and risks

1. Build the mascot with its own wgpu surface on a winit loop it shares with eframe (`create_native` /
   `EframeWinitApplication`, section 6), so it avoids eframe's root-viewport constraint. A separate process is the
   fallback if the shared loop fails the run-list checks. On Windows, use `DxgiFromVisual` for the surface (section 3.2).
2. Use `WS_EX_TOPMOST`-equivalent `AlwaysOnTop`, skip-taskbar on Windows, and `Accessory` only if the app can accept
   a Dock-less main window. Confirm each on a machine.
3. Start with whole-window click-through (section 5.1). Add per-pixel click-through only after the run-list shows the
   polling cost is acceptable.
4. Opacity in the shader (section 4.4).
5. Wayland is a separate problem: no always-on-top in winit and no taskbar hint. Per-pixel click-through is planned as
   the alpha-mask input region (section 5.2), which is *not verified*; if it fails on a compositor, plan a
   degraded mode with whole-window click-through.

**Risks:**
- DX12 alpha depends on `DxgiFromVisual` working on the target machines; the default `DxgiFromHwnd` is opaque.
  Its end-to-end result is not verified.
- Vulkan alpha caps vary by driver, so a machine matrix is needed before any choice is final.
- Metal `PostMultiplied` encoding may give a halo over a white or black desktop.
- Wayland always-on-top depends on the compositor's layer-shell support.

## 9. Run-list for the follow-up

These steps need a real display, GPU or compositor, so they need a machine with one. They go in
`spikes/mascot-window/` (its own `Cargo.lock`, winit 0.30.13 and wgpu 30.0.1, not in the workspace).

1. **`caps` bin:** print the adapter and `get_capabilities().alpha_modes` for each backend, selected with
   `WGPU_BACKEND=dx12|vulkan|metal|gl`. Answers section 3.1 for Vulkan and GL.
2. **`window` bin:** draw the scene.wgsl test picture in a transparent window, with flags for level
   (`--level`), skip-taskbar (`--skip-taskbar`), opacity (`--opacity`) and click-through
   (`--passthrough whole|pixel`), and DX12 swapchain kind (`--dx12-swapchain hwnd|visual`, set through the wgpu
   backend options). Run both as a standalone window and inside a shared `EframeWinitApplication` loop (section 6).
   Answers sections 3.2, 4 and 5 on each OS.
3. **`egui-viewport` bin:** a transparent root and a transparent secondary viewport, to test section 6.
4. **`cost` bin:** 1080p at 30 fps for 60 s, GPU timestamps and CPU percentage, for the cost question.
5. **Record per machine:** OS build, GPU and driver, desktop and compositor, `XDG_SESSION_TYPE`.
6. **Screenshots** of the window over solid white and solid black, for the fringe check (section 7).

Open questions that only the run-list can answer: Vulkan alpha caps per driver; the DX12 composition path working
end to end; fringe over white and black on Windows and macOS; Alt-Tab and Dock hiding; GNOME, KDE and sway behaviour;
cost at 1080p30.

## 10. Sources

Local crate sources (pinned, read after `cargo fetch`):

- `wgpu-types-30.0.1/src/surface.rs:114-132`
- `wgpu-30.0.1/src/api/surface.rs:437-443`
- `wgpu-types-30.0.1/src/backend.rs:675-690` (`Dx12SwapchainKind`), `wgpu-hal-30.0.1/src/dx12/instance.rs:142-147`
- `egui-wgpu-0.36.2/src/setup.rs:124, 289` (backend options passed through)
- `winit-0.30.13/src/event_loop.rs:69, 117-119` (single event loop per process)
- `eframe-0.36.2/src/lib.rs:376` (`create_native`), `eframe-0.36.2/src/native/run.rs:486-492` (`EframeWinitApplication`)
- `winit-0.30.13/src/platform_impl/linux/wayland/window/mod.rs:585-598` (hittable: `None`; not hittable: empty region)
- `wgpu-hal-30.0.1/src/dx12/adapter.rs:1364-1375`
- `wgpu-hal-30.0.1/src/auxil/dxgi/conv.rs:293-297`
- `wgpu-hal-30.0.1/src/vulkan/swapchain/native.rs:178`, `src/vulkan/conv.rs:542-557`
- `wgpu-hal-30.0.1/src/metal/adapter.rs:468-471`, `src/metal/surface.rs:268-271`
- `wgpu-hal-30.0.1/src/gles/adapter.rs:1376`
- `egui-wgpu-0.36.2/src/winit.rs:272-290`
- `eframe-0.36.2/src/native/wgpu_integration.rs:228`
- `egui-0.36.2/src/viewport.rs:306-325`
- `egui-winit-0.36.2/src/lib.rs:1935-1938, 2068, 2077, 2184-2192, 2236-2238`
- `winit-0.30.13/src/window.rs:936-946, 1179-1183, 1798-1803`
- `winit-0.30.13/src/platform/macos.rs:257-268`, `src/platform/x11.rs:165`, `src/platform/windows.rs:489, 500, 560-573`
- `winit-0.30.13/src/platform_impl/windows/window_state.rs:273-277, 300-301, 346`
- `winit-0.30.13/src/platform_impl/windows/window.rs:632, 1230-1243, 1498`
- `winit-0.30.13/src/platform_impl/linux/x11/window.rs:241-246, 1073-1074, 1623-1656`
- `winit-0.30.13/src/platform_impl/linux/wayland/window/mod.rs:430, 585-598`
- `winit-0.30.13/src/platform_impl/macos/window_delegate.rs:1188, 1531-1535`
- `winit-0.30.13/src/platform_impl/macos/event_loop.rs:229`

Repository files:

- `spikes/broadcast-out/spout-out/src/scene.wgsl`, `spikes/broadcast-out/README.md`
- `docs/design/settings.md:190` (`mascot.*` keys; click-through and on-top need Confirm)
- Related (OS input capture; cursor polling for per-pixel click-through)

*Not verified* sources, for the follow-up to check: docs.rs for wgpu and winit, Microsoft Learn for
`WS_EX_TOOLWINDOW` and DirectComposition, Apple's `NSWindow` and `CAMetalLayer` references, Mesa's WSI issue tracker,
the wlr-layer-shell and compositor support matrices, and the wgpu and winit issue trackers for composite-alpha bugs.
