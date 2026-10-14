# Capturing keyboard, mouse, pen and gamepad input outside the app (2026-10-12)

> Research pass (PF-29 input-driven body motion, PF-05 bindings, PF-25 triggers; PF-30 reuses the same layer). Checked 2026-10-12 against vendor docs, crate manifests and repos. "(unverified)" means I found no primary source and did not test it. Nothing here was built or run on Windows or macOS: every behaviour claim is from documentation or source, not from a measurement. The constraint is the spec's: "no game hooks, only OS input the user allows (scope-gated by the `input` scope)", and nothing non-commercial (`docs/licensing/policy.md`).

## 0. Summary

| | Keyboard | Mouse | Pen | Gamepad |
|---|---|---|---|---|
| **Windows** | Raw Input, `RIDEV_INPUTSINK`, own hidden window | same | position, pressure and tilt all **unverified unfocused**; candidates are mouse Raw Input (position) and the Raw Input HID digitiser (pressure, tilt); a hardware spike will decide | XInput polling; Raw Input HID for non-XInput pads. **Not** Windows.Gaming.Input (needs focus) |
| **macOS** | listen-only `CGEventTap` (Input Monitoring) | same | tablet events through the same tap **unverified**; spike | `GCController` with `shouldMonitorBackgroundEvents = true`; fall back to `IOHIDManager` (gilrs) |
| **Linux X11** | XInput2 raw events on the root window (`x11rb`) | same | XInput2 raw valuators (pressure, tilt) **unverified; needs a hardware spike** | `gilrs` (evdev) |
| **Linux Wayland** | no sanctioned passive path. GlobalShortcuts portal for PF-05 hotkeys; opt-in evdev (`input` group) for PF-29 typing | opt-in evdev only | opt-in evdev only | `gilrs` (evdev, `uaccess` ACL on most distros) |

Crates to use: `windows`, `objc2` + `objc2-core-graphics` / `objc2-game-controller` / `objc2-io-kit`, `x11rb`, `ashpd`, `evdev`, `gilrs` (with the `xinput` feature on Windows), SDL_GameControllerDB (through gilrs). Crates to reject: `rdev`, `device_query`.

The one common design rule: the capture layer turns input into an in-memory stream of normalised events, keeps nothing on disk, sends nothing off the machine, and runs only while the `input` scope is granted. Windows and Linux X11 show the user nothing when an app listens (see 5), so Omuzaki must show its own indicator.

## 1. APIs per OS and device

### 1.1 Windows

**Keyboard and mouse: Raw Input with `RIDEV_INPUTSINK`.**
- `RegisterRawInputDevices` with usage page 0x01, usage 0x06 (keyboard) and 0x02 (mouse). By default only the focused window gets raw input; `RIDEV_INPUTSINK` "enables the caller to receive the input even when the caller is not in the foreground", and `hwndTarget` must then name a window ([RAWINPUTDEVICE](https://learn.microsoft.com/en-us/windows/win32/api/winuser/ns-winuser-rawinputdevice)). So we need a window; it can be our own hidden window (message-only windows: unverified, test).
- `RIDEV_EXINPUTSINK` is a weaker variant: background input only when the foreground app didn't register for raw input (same page). Do not use it: a game that registers for raw input would starve us.
- Delivery is `WM_INPUT`; the buffered method (`GetRawInputBuffer`) drains many events per wake and is meant for 1000 Hz mice ([Raw Input overview](https://learn.microsoft.com/en-us/windows/win32/inputdev/about-raw-input)).
- Raw Input needs no DLL, no injection and no hook chain entry. The data is the OS delivering to our own queue.
- Microsoft itself steers hook users here: "In most cases where the application needs to use low level hooks, it should monitor raw input instead. This is because raw input can asynchronously monitor mouse and keyboard messages that are targeted for other threads more effectively than low level hooks can" ([LowLevelKeyboardProc](https://learn.microsoft.com/en-us/windows/win32/winmsg/lowlevelkeyboardproc)).

**Why not `WH_KEYBOARD_LL` / `WH_MOUSE_LL`.** They work unfocused and are not injected: "the `WH_KEYBOARD_LL` hook is not injected into another process. Instead, the context switches back to the process that installed the hook" ([same page](https://learn.microsoft.com/en-us/windows/win32/winmsg/lowlevelkeyboardproc)). So they would satisfy "nothing loads into another process". They lose on three counts, all from that page:
- Every key event waits on our thread: if the hook proc is slower than `LowLevelHooksTimeout` (capped at 1000 ms on Windows 10 1709+) "the hook is silently removed without being called" with no notice to the app.
- They sit in the input path of every app, not beside it, so a slow or stuck proc delays the user's typing in the game. Microsoft: "Hooks tend to slow down the system" and global hooks "hurt system performance and cause conflicts" ([Hooks overview](https://learn.microsoft.com/en-us/windows/win32/winmsg/about-hooks)).
- They are the API keyloggers use, so they are the shape of thing that anti-cheat and antivirus look at (see 3; the detection claim itself is unverified).
- The non-LL hooks (`WH_KEYBOARD`, `WH_GETMESSAGE` and the rest) for other processes' threads need a DLL in the target: "If the application installs a hook procedure for a thread of a different application, the procedure must be in a DLL" (hooks overview). That is exactly what the spec forbids. Never use them.

**Pen.**
- `WM_POINTER*` is no use unfocused: pointer messages go to the window the pointer is over, and to the capturing window while in contact ([WM_POINTERUPDATE](https://learn.microsoft.com/en-us/windows/win32/inputmsg/wm-pointerupdate)).
- Windows lists pen digitisers (usage page 0x0D, usage 0x01 external pen, 0x02 integrated pen) as supported top-level collections that the Raw Input Manager opens exclusively ([HID architecture](https://learn.microsoft.com/en-us/windows-hardware/drivers/hid/hid-architecture)). Raw Input can register for a usage page and usage. Whether `WM_INPUT` then delivers pen reports (pressure, tilt) to a background sink window for a Windows Ink pen: **unverified**; I found no Microsoft statement either way. Spike needed on a Wacom and a Surface-class pen.
- Possible fallback, **unverified**: a pen may also drive the system cursor, so mouse Raw Input could give position and buttons. Microsoft only says unhandled pointer input "may generate mouse input" (WM_POINTERUPDATE page); it does not say this happens for an unfocused sink window. If it holds, that gives arm-to-pen position but not pressure or tilt. A hardware spike decides it.
- Wacom's own Wintab driver API: not evaluated; likely a vendor SDK with its own terms (flag for a later licence check, do not adopt).

**Gamepad.**
- **XInput**: polled, up to four pads, `XInputGetState` plus `dwPacketNumber` to detect change; Microsoft says not to poll empty slots every frame, check for new pads every few seconds ([XInput getting started](https://learn.microsoft.com/en-us/windows/win32/xinput/getting-started-with-xinput)). The page does not say whether XInput works for a background process. The `gilrs` maintainers document the opposite problem for the newer API and tell windowless programs to use XInput (next bullet), which is indirect evidence, not a Microsoft statement. Test on hardware.
- **Windows.Gaming.Input**: do not use for this. gilrs: "Windows Gaming Input requires an in focus window to be associated with the process to receive events" ([gilrs README](https://gitlab.com/gilrs-project/gilrs/-/blob/master/README.md)). `gilrs` defaults to this backend, so we must turn default features off and enable `xinput`.
- **GameInput** (Microsoft's new API, NuGet `Microsoft.GameInput`): a "functional superset" of XInput, DirectInput, Raw Input and WinRT, Windows 10 19H1 and later ([GameInput introduction](https://learn.microsoft.com/en-us/gaming/gdk/_content/gc/input/overviews/input-overview)). The page does not say how it behaves for an unfocused process, and the redistribution licence of the NuGet runtime is not checked. **Unverified, do not adopt yet**; it is the natural v2 once someone tests focus behaviour and reads the licence.
- **Raw Input HID** for pads XInput doesn't cover (PlayStation, Switch Pro, generic): usage page 0x01 usages 0x04 (joystick) and 0x05 (gamepad) are supported "Game controllers", access mode "Shared" (HID architecture page). Register them with `RIDEV_INPUTSINK` through the same window. Report parsing is per-device; gilrs/SDL mappings are the practical route, so use Raw Input HID only if gilrs misses a device.

**What winit gives unfocused (Windows)** (source, branch `v0.30.x`; I did not read the 0.31 beta): with `DeviceEvents::Always`, winit registers mouse and keyboard Raw Input with `RIDEV_DEVNOTIFY | RIDEV_INPUTSINK`; otherwise only `RIDEV_DEVNOTIFY` ([`raw_input.rs`](https://github.com/rust-windowing/winit/blob/v0.30.x/src/platform_impl/windows/raw_input.rs)). The default is `WhenFocused` ([`event_loop.rs`](https://github.com/rust-windowing/winit/blob/v0.30.x/src/event_loop.rs)). So winit can already give unfocused raw mouse motion and keys on Windows if we call `listen_device_events(Always)`. It registers only the keyboard and mouse collections: no pen, no gamepad. It would also tie capture to the render window's event loop and its lifetime. Our own capture thread, with its own window, is cleaner and keeps the scope gate in one place.

### 1.2 macOS

**Keyboard, mouse (and maybe pen): listen-only `CGEventTap`.**
- A tap created with `kCGEventTapOptionListenOnly` can observe, not modify. Per an Apple DTS answer on the developer forums, an active (default) tap triggers the Accessibility request and a listen-only tap triggers the **Input Monitoring** request; an app that already holds Accessibility doesn't get the second prompt; Input Monitoring is available to sandboxed apps ([forum thread 122492](https://developer.apple.com/forums/thread/122492), via search summary; I could not fetch the thread text, so treat the exact wording as secondhand).
- Check and request calls: `CGPreflightListenEventAccess` and `CGRequestListenEventAccess` (same thread, DTS).
- Known rough edges, all forum reports: the prompt shows once per install and may not list the app ([thread 828052](https://developer.apple.com/forums/thread/828052)); the tap returns nil before permission is granted (same thread). rdev's tracker has a fix titled "recover disabled macOS event taps", i.e. taps get disabled by the system and must be re-enabled; Apple's doc for that (`kCGEventTapDisabledByTimeout`) I did not fetch.
- Tablet/pen events through a listen-only tap: unverified. Spike on a Wacom.

**`IOHIDManager` instead.** It reads devices below the window server. It also needs Input Monitoring: "using `IOHIDManagerCopyDevices` to get a list of keyboards and mice triggers the user facing Input Monitoring request", and SDL had to stop enumerating HID devices when permission is missing ([SDL discourse](https://discourse.libsdl.org/t/sdl-dont-enumerate-hid-devices-on-macos-if-we-dont-have-input-monitoring-permissions/38822), via search summary). Apple's open IOKit source maps `IOHIDRequestAccess` / `IOHIDCheckAccess` with the listen-event type to the same TCC service ([IOHIDLib.c](https://opensource.apple.com/source/IOKitUser/IOKitUser-1726.41.1/hidsystem.subproj/IOHIDLib.c.auto.html)). Forum posts say `IOHIDCheckAccess` returns wrong answers in places ([thread 809431](https://developer.apple.com/forums/thread/809431)). So it is no easier on permissions, and it hands us raw HID reports to parse. Use it only where the tap can't see a device (pens, odd pads).

**Gamepad: `GameController.framework`.** `GCController.shouldMonitorBackgroundEvents` is "whether the app needs to respond to controller events when it isn't the frontmost app" ([Apple docs](https://developer.apple.com/tutorials/data/documentation/gamecontroller/gccontroller/shouldmonitorbackgroundevents.md)). The default flipped to off for apps built against macOS 11.3 or later, so we must set it. macOS 15.4 broke it (the property read back false); Apple said that was unintentional and reported it fixed in 15.5 ([forum thread 782461](https://developer.apple.com/forums/thread/782461)). This avoids the Input Monitoring prompt in principle; whether it avoids it in practice: unverified. Rust bindings: `objc2-game-controller`. `gilrs` on macOS goes through IOKit HID (its `gilrs-core` manifest enables `objc2-io-kit` `hid`), so it likely triggers the Input Monitoring prompt: unverified.

**What winit gives unfocused (macOS):** nothing useful. `listen_device_events` is documented unsupported on macOS, and winit's device events come from overriding `NSApplication sendEvent:` ([`app.rs`](https://github.com/rust-windowing/winit/blob/v0.30.x/src/platform_impl/macos/app.rs)), so it only sees events the OS sends to our app.

### 1.3 Linux X11

- **XInput2 raw events** (`XI_RawKeyPress`, `XI_RawButtonPress`, `XI_RawMotion`, selected on the root window for `XIAllMasterDevices`). The protocol: "RawEvents are sent exclusively to all root windows", and "Clients supporting XI 2.1 or later receive raw events at all times, even when the device is grabbed by another client" ([XI2proto](https://xorg.freedesktop.org/archive/current/doc/inputproto/XI2proto.txt)). So they ignore focus. Raw events carry valuators; whether they include pen pressure and tilt for a tablet device is from my knowledge of XI2 and **not checked here** (needs a hardware spike).
- No injection into other clients; we are one more client of the X server. Binding: `x11rb` with its `xinput` feature (manifest, [x11rb Cargo.toml](https://github.com/psychon/x11rb/blob/master/x11rb/Cargo.toml)).
- XRecord is the older route (rdev uses it); XI2 is better: event-based, with device ids and valuators.
- **winit on X11:** it selects `XI_RawKey*`, `XI_RawButton*` and `XI_RawMotion` on the root ([`event_processor.rs`](https://github.com/rust-windowing/winit/blob/v0.30.x/src/platform_impl/linux/x11/event_processor.rs)) but "since the DeviceEvent capture can lead to high CPU usage for unfocused windows, winit will ignore them by default for unfocused windows on Linux/BSD"; `listen_device_events(Always)` turns them back on ([`event_loop.rs`](https://github.com/rust-windowing/winit/blob/v0.30.x/src/event_loop.rs)). That is a vendor warning about idle cost, and a reason to filter in our own thread rather than take every event.

### 1.4 Linux Wayland

Wayland deliberately gives apps no view of input they don't have focus for (see the LWN analysis and the Mutter issue below). So:
- **Global shortcuts portal** (`org.freedesktop.portal.GlobalShortcuts`): "These shortcuts are activated regardless of the focused state of the application window", with Activated/Deactivated signals carrying a timestamp, and a dialog where the user reviews the shortcuts ([portal docs](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.GlobalShortcuts.html)). It covers PF-05 and PF-25 hotkeys (a named shortcut, press and release). It cannot drive per-key typing animation, because we only hear about shortcuts we registered. Compositor support varies (GNOME and KDE implement it per the forum summaries; which versions: unverified). Binding: `ashpd` has the `global_shortcuts` module (docs.rs).
- **InputCapture portal**: not a fit. It "lets an application receive input from devices that would otherwise move the on-screen pointer" over EIS/libei, triggered by pointer barriers at screen edges, and the compositor "can filter events or stop capturing at any time" ([portal docs](https://flatpak.github.io/xdg-desktop-portal/docs/doc-org.freedesktop.portal.InputCapture.html)). It is for sharing one keyboard across machines, not passive listening.
- **XWayland**: not reliable. Per the GNOME Mutter tracker, XWayland clients "can only read keyboard input if another XWayland program is focused" and KDE allows more only if the user opts in ([mutter#2565](https://gitlab.gnome.org/GNOME/mutter/-/issues/2565), [gnome-shell#6170](https://gitlab.gnome.org/GNOME/gnome-shell/-/issues/6170), via search summary).
- **evdev with the `input` group, a documented opt-in**: read `/dev/input/event*` directly. Needs membership of the `input` group (or a udev rule); returns key codes, not characters. Only anecdotal sources on this ([a developer write-up](https://dev.to/vl360/auto-correcting-wrong-layout-typing-on-wayland-is-nearly-impossible-we-did-it-anyway-j19), unverified). The `input` group can read every keyboard, so this is a real security downgrade for the user; make it a visible opt-in with an explanation, never a silent fallback. Works on X11 and Wayland alike, which makes it the single Linux fallback.
- **Pen on Wayland** has no passive path either: evdev opt-in only.
- **winit on Wayland**: `listen_device_events` is an empty function in the Wayland backend (source: `platform_impl/linux/wayland/event_loop/mod.rs`, v0.30.x). It gives nothing unfocused.

### 1.5 Linux gamepad (X11 and Wayland)
`gilrs` reads `/dev/input/event*` directly (evdev) and needs read access; "on most distros it shouldn't be a problem", otherwise a udev rule ([gilrs README](https://gitlab.com/gilrs-project/gilrs/-/blob/master/README.md)). It needs `libudev` development files to build. Gamepads work the same on both display servers because they never go through the compositor.

## 2. What the spec needs from the layer
- PF-29 needs press and release per key and per button, mouse motion deltas and/or position, stick axes, optionally pen position and pressure.
- PF-05/PF-25 need the same for bindings, plus modifier state.
- PF-30 adds nothing new.
- One normalised event type, one trait per OS backend, one scope gate. The backend reports its limits (for example `PenPressure: unavailable`) so the UI can say so.

## 3. Anti-cheat

**What I can and can't document.** Vendors publish little about what their client detects. The statements below are all I found; everything else is marked unverified. I did not test any game.

| System | What the vendor says | Reading for us |
|---|---|---|
| **BattlEye** | "Generally we only ever ban for the use of actual cheats/hacks or components of such hacks"; non-cheat overlays "are generally supported" unless the game says otherwise; "We might decide to kick (not ban) you at some point for using a specific program (such as macro tools)"; it blocks software using kernel drivers with known security issues; it tells users to remove `d3d9.dll`, `dxgi.dll`, `dsound.dll` from the game folder, and a blocked file means "the game process tried to load them" ([BattlEye FAQ](https://www.battleye.com/support/faq/)). | The risk named is code loaded into the game and macro tools, which is injection and synthesised input. Passive listening from our own process is neither. No statement either way about Raw Input listeners (unverified). |
| **EAC** | No primary source found. The EOS docs page returned empty, the support site 404'd. Third-party reports say it blocks some overlays and utilities and does not promise compatibility with every tool ([guru3d](https://www.guru3d.com/story/update-watch-dogs-2-anti-cheat-system-blocks-rtss-overlay-software/), [EA forum](https://forums.ea.com/discussions/apex-legends-technical-issues-en/does-apex-legends-not-like-overlays/5204296), anecdotal). | Unverified. Those reports are about overlays and hardware tools that inject or load drivers. |
| **Vanguard** | Riot's FAQ: "There is absolutely no allow list for Vanguard", and overlays or tools using the API and game client "should continue to function"; external tools reading memory stop working ([Riot FAQ](https://www.riotgames.com/en/news/vanguard-faq)). Secondary reports: the kernel driver blocks other drivers with known vulnerabilities and Riot says it doesn't send machine data back ([PC Gamer, 2020](https://www.pcgamer.com/uk/the-controversy-over-riots-vanguard-anti-cheat-software-explained/), search summary). I found no source about input listeners or hooks. | Unverified. No allow list means "ask for an exemption" is not an option, so the design must need no exemption. |

**Listening vs hooks vs injection, as far as the sources reach.**
- *Injection / loading into the game:* what BattlEye names, what EAC and Vanguard exist to stop, and what the spec forbids. We do none of it.
- *Synthesised input* (`SendInput`, `CGEventPost`, uinput): BattlEye says macro tools can get a player kicked. Omuzaki must not simulate input in any capture path. This rules out rdev's `simulate` and `grab`, which ship in that crate.
- *Global LL hooks:* not injection (Microsoft), but they live in the input path and are a keylogger shape. No vendor says they are flagged. Treat as higher risk than Raw Input, unverified in both directions.
- *Raw Input / XInput polling / XI2 / CGEventTap listen-only:* the OS delivers to us; nothing is loaded into the game. I found no vendor statement that any anti-cheat flags these. The honest wording for users: "Omuzaki only reads input the OS gives it, like OBS hotkeys. It never loads into or touches your game. We can't promise every anti-cheat agrees; none of the three vendors documents objecting to this."

**Evidence that this class is common:** VMagicMirror, a VRM app, reads keyboard through the `RawInput.Sharp` library and gamepads through SharpDX DirectInput (its [README](https://github.com/malaybaku/VMagicMirror/blob/master/README.md) dependency list), and a user reports on a forum that on Windows the character keeps tracking when another app covers it ([EndeavourOS forum](https://forum.endeavouros.com/t/endeavour-os-not-tracking-keyboard-on-vmagicmirror/67644), a user report). That shows it ships and works; it says nothing on any anti-cheat.

**Mitigations to build in:**
1. An in-app "Input capture" panel that shows what is being read (device classes, not key content) and a one-click off.
2. A kill switch that stops all capture when a configurable list of protected game executables is running (default off, listed in docs). Cheap and gives a clean answer for risk-averse users.
3. A public compatibility list, filled from user reports, not claimed by us.
4. Open a ticket with each vendor's support to ask about Raw Input listeners (see 8, risk 1).

## 4. Crates

Licence column is the `license` field of the crate's own manifest, read from crates.io on 2026-10-12, checked against the repo's manifest or licence file where noted. Maintenance is from crates.io and the repo host API.

| Crate | Licence (manifest) | Last release | Repo state | Verdict |
|---|---|---|---|---|
| `windows` 0.62.2 | MIT OR Apache-2.0 | 2025-10-06 | [microsoft/windows-rs](https://github.com/microsoft/windows-rs): pushed daily, 48 open issues | **Use.** Raw Input, XInput (`Win32_UI_Input_XboxController`), HID. |
| `windows-sys` 0.61.2 | MIT OR Apache-2.0 | 2025-10-06 | same repo | Allowed alternative (raw bindings, no COM helpers). Pick one of `windows` / `windows-sys` per crate to avoid duplicates. |
| `objc2` 0.6.5 | MIT | 2026-10-05 | [madsmtm/objc2](https://github.com/madsmtm/objc2): pushed 2026-10-12, 105 open issues | **Use.** |
| `objc2-core-graphics` 0.3.2 | Zlib OR Apache-2.0 OR MIT | 2025-10-04 | same repo | **Use.** `CGEventTap`. |
| `objc2-game-controller` 0.3.2 | Zlib OR Apache-2.0 OR MIT | 2025-10-04 | same repo; only ~19k downloads | **Use**, with a hardware spike (generated bindings, few users). |
| `objc2-io-kit` 0.3.2 | Zlib OR Apache-2.0 OR MIT | 2025-10-04 | same repo | Use only for the IOHID fallback. |
| `x11rb` 0.14.0 | MIT OR Apache-2.0 | 2026-07-16 | [psychon/x11rb](https://github.com/psychon/x11rb): pushed 2026-09-06, 53 open issues | **Use**, `xinput` feature. Pure-Rust connection by default; the libxcb option is not needed. |
| `ashpd` 0.13.13 | MIT | 2026-07-17 | [bilelmoussaoui/ashpd](https://github.com/bilelmoussaoui/ashpd): pushed 2026-09-29, 19 open issues | **Use** for the GlobalShortcuts portal. |
| `evdev` 0.13.2 | Apache-2.0 OR MIT | 2025-09-15 | [cmr/evdev](https://github.com/cmr/evdev) (the GitHub API returned "moved permanently"; repo state not checked) | **Use** for the opt-in evdev path. Check the new repo location before registering. |
| `gilrs` 0.11.2 / `gilrs-core` 0.6.8 | Apache-2.0/MIT | 2026-05-30 | [GitLab gilrs-project](https://gitlab.com/gilrs-project/gilrs): last activity 2026-10-05 | **Use**, gamepads only, with `default-features = false, features = ["xinput"]` on Windows (the default `wgi` backend needs window focus). Linux build needs libudev dev files (`libudev-sys`). On macOS it uses IOKit HID (permission prompt likely). `gilrs-core` accepts `windows` `>=0.44, <=0.62`, so keep our `windows` pin in that range or we compile two copies. |
| SDL_GameControllerDB | Zlib (LICENSE file, Sam Lantinga text, [repo](https://github.com/mdqinc/SDL_GameControllerDB)) | n/a (data file) | pushed 2026-10-02, 3 open issues | **Use**, through the gilrs submodule (gilrs `.gitmodules` points at `gabomdq/SDL_GameControllerDB`, which now redirects). Keep the Zlib notice. Per-mapping provenance is crowd-sourced and not stated in the repo: low risk, record it. |
| `winit` 0.31.0-beta.3 | Apache-2.0 | 2026-09-04 | [rust-windowing/winit](https://github.com/rust-windowing/winit): active, 711 open issues | Already in the stack for windowing. **Do not** rely on its `DeviceEvent`s for capture (1). |
| `rdev` 0.5.3 | MIT | **2023-06-26** (git `main` says 0.6.0, unreleased) | [Narsil/rdev](https://github.com/Narsil/rdev): last commit 2026-05-12; 61 open issues | **Reject.** See below. |
| `device_query` 4.0.1 | MIT | 2025-07-21 | [ostrosco/device_query](https://github.com/ostrosco/device_query): **archived on GitHub**, README says it moved to Codeberg | **Reject.** See below. |
| `willhook` 0.6.3, `inputbot` 0.6.0 | MIT | 2023 | not checked | Reject on sight: both are hook wrappers by name and description; I did not read the code. |
| `hidapi` 2.6.7 | MIT (crate); the C library has its own choice of licence | 2026-08-27 | active | Not needed. If ever used, take the BSD option for the C library and record it. |

**Why `rdev` is rejected.**
- Windows uses global low-level hooks: `SetWindowsHookExA(WH_KEYBOARD_LL …)` and `WH_MOUSE_LL` ([`windows/common.rs`](https://github.com/Narsil/rdev/blob/main/src/windows/common.rs)). Section 1.1 says why we skip them.
- Linux listening uses XRecord via the `x11` crate, and Wayland needs extra features (`wayland`, `unstable_grab`), per its [Cargo.toml](https://github.com/Narsil/rdev/blob/main/Cargo.toml) and README ("will not work in Wayland").
- It bundles `simulate` and `grab` (injection) in the same crate. We would link code whose job is input injection. That is the opposite of the spec's promise and a bad look under an anti-cheat scan (the scan claim is unverified; the bundling is fact).
- The published release is three years old; `main` has moved on to 0.6.0 with an edition-2024 manifest but no release. Open issues include crashes and thread-safety (an Electron crash on macOS; "Thread safety violation in `windows::grab::grab`"; "loses the permission token on Wayland").
- It cannot do pen or gamepad.
- Its only value is "one call for three OSes"; our own three small backends are less code than working around it.

**Why `device_query` is rejected.**
- It polls state ("query mouse and keyboard inputs on demand"), so it gives no events, no press timing between polls, and a wake-up cost at every poll rate. PF-29 wants key down/up.
- The repo is archived and moved; the last release is July 2025.
- It pins `windows` 0.48 (its Cargo.toml), which would pull a second copy of the Windows bindings.
- macOS needs Accessibility ("only see meta keys such as shift" otherwise, per its README), not just Input Monitoring.

## 5. Privacy: prompts and indicators

| OS | What the user is asked | What stays visible while we listen | Source |
|---|---|---|---|
| **Windows** | Nothing for Raw Input, hooks, XInput. No OS permission found in Microsoft's docs for these. | No OS indicator found. | Raw Input, hooks pages above; absence of evidence, not a documented guarantee |
| **macOS** | A one-time Input Monitoring prompt (listen-only tap, IOHID). Apple: "Some apps can monitor your keyboard, mouse, or trackpad even when you're using other apps"; the user turns each app on or off in System Settings, Privacy & Security, Input Monitoring ([Apple Support](https://support.apple.com/guide/mac-help/control-access-to-input-monitoring-on-mac-mchl4cedafb6/mac)). | The Apple page mentions no on-screen indicator. | same |
| **Linux X11** | Nothing. Any client can select raw events. | None. | XI2 protocol |
| **Linux Wayland** | GlobalShortcuts: a portal dialog to review the shortcuts. Evdev: a group membership, set outside the app. | Portal-dependent. | portal docs; evdev write-up |

So on Windows and X11 the only protection for the user is Omuzaki's own behaviour. That is why Omuzaki shows its own indicator and an explanation (the rule in section 0). We take Apple's wording as the model for the explanation text.

**How the incumbents explain it.**
- **VSeeFace** ([site](https://www.vseeface.icu/)): says "All configurable hotkeys also work while it is in the background or minimized" and "On some systems it might be necessary to run VSeeFace as admin to get this to work properly". It documents no keylogger or privacy statement, and only mentions antivirus as over-eager deleting of its files. It does not document an input-to-pose feature; its hand tracking needs a Leap Motion. So VSeeFace is hotkeys only. The admin note may point to Windows' rule that a non-elevated process doesn't see elevated apps' input: the cause is not stated (unverified).
- **VMagicMirror**: marketed as moving the avatar from "only your keyboard and mouse", with game controller and MIDI support ([site](https://malaybaku.github.io/VMagicMirror/en/)). I could not find a page that explains how it reads keys or what it does with them; the README lists `RawInput.Sharp` and SharpDX DirectInput as dependencies. No privacy statement found.
- **Warudo**: the [Keyboard/Trackpad asset page](https://docs.warudo.app/docs/assets/keyboard) explains the feature (a prop plus a blueprint, only for upper-body tracking setups) and says nothing on permissions, focus, antivirus or anti-cheat. No statement found anywhere else in the docs I could reach.

Conclusion: none of the three explains the privacy side. Omuzaki can do better by saying plainly what is read (key identity in memory, only while the scope is on), what is not (no log, no network, no clipboard), and showing an indicator. That is a trust differentiator, with a research cost of nothing.

## 6. Latency and idle CPU

I found no published delivery-latency or idle-CPU figure for any of these APIs, and I have measured nothing. What the sources do give:

| Path | Delivery model | What follows |
|---|---|---|
| Windows Raw Input | Event-driven `WM_INPUT` to our queue; buffered read for high-rate mice | Idle cost is a blocked `GetMessage`. Cost grows with event rate only. |
| Windows LL hook | Our thread is sent a message per event; 1000 ms max before silent removal | Adds latency to the user's own input if we are slow. |
| XInput | **Polled** | Polling rate sets both latency and idle CPU. At 120 Hz the worst-case added latency is 8.3 ms. Choose from the avatar frame rate; XInput change detection by `dwPacketNumber` is free. Skip empty slots (Microsoft). |
| macOS tap | Event-driven on a run loop | Tap may be disabled by the system if the callback is slow (rdev shows this happens); keep the callback tiny and hand off. |
| GameController | Event-driven handlers (not measured) | |
| X11 XI2 raw | Event-driven on the X connection | winit says unfocused device events can mean "high CPU usage" (so filter early, and unselect `XI_RawMotion` when no mouse binding needs it). |
| evdev / gilrs | Event-driven on `epoll` / `inotify` | gilrs's Linux backend uses `inotify` for hotplug (its manifest) and has no polling timer I could find; not verified. |

**Proposed budget (for the maintainers and §10 to rule on, not an edit):** a new row, `input.capture`, "OS event received to normalised event queued on the avatar thread", ≤ 1 ms p95, and capture thread CPU ≤ 0.2 % of 8 cores on `R` with a keyboard, a mouse at 1000 Hz and one gamepad polled at 120 Hz. Both numbers are guesses to be replaced by the spike in section 7; they fit inside `cpu.total` ≤ 3 % and `latency.laptop` ≤ 80 ms. Mouse at 1000 Hz is the stress case: coalesce motion on the capture thread to the avatar frame rate.

## 7. Recommended approach

**One crate, `omuzaki-input` (proposed name), with one backend per OS behind a small trait.** It owns the scope gate, the indicator state, the kill switch and event normalisation. The rest of the studio sees only normalised events.

| OS | Keyboard + mouse | Pen | Gamepad |
|---|---|---|---|
| **Windows** | Raw Input + `RIDEV_INPUTSINK` on a hidden window, buffered read, `windows` crate. No hooks. | Pen is unverified until the hardware spike: mouse Raw Input for position if it shows the pen drives it unfocused; Raw Input HID digitiser for pressure/tilt if it delivers. | `gilrs` with `xinput`; Raw Input HID fallback for devices gilrs misses; no Windows.Gaming.Input. |
| **macOS** | Listen-only `CGEventTap` on its own run-loop thread, `CGRequestListenEventAccess`, re-enable on disable. `objc2-core-graphics`. | Tablet events through the tap if the spike shows them, else IOHID. | `objc2-game-controller` with `shouldMonitorBackgroundEvents = true`; guard for the macOS 15.4 regression; `gilrs` IOKit as fallback. |
| **Linux X11** | XI2 raw events (`x11rb`), root window, all master devices; subscribe only to the classes bound. | XI2 raw valuators (pressure, tilt) if the hardware spike confirms. Pen position only if it confirms background pen-position delivery; otherwise report pen input unavailable on X11. | `gilrs`. |
| **Linux Wayland** | PF-05/PF-25 hotkeys: GlobalShortcuts portal (`ashpd`). PF-29 typing and mouse: opt-in evdev (`evdev` crate) with an explicit explainer and a `input` group check; otherwise the feature says it is unavailable. | Opt-in evdev. | `gilrs`. |

**Spikes before the design is final (each a small task, needs hardware):**
1. Pen, all three OSes: needs the hardware spike. Windows: does a background sink window get Windows Ink pen reports (pressure, tilt) from Raw Input? Does XInput report while an unfocused, borderless game runs? Does elevated-game input reach a non-elevated sink (UIPI)?
2. macOS: is any Input Monitoring prompt shown for `GCController` with background events? Do tablet events reach a listen-only tap?
3. Linux: what does evdev need on KDE and GNOME Wayland; do GlobalShortcuts portals exist on the compositors we target?
4. Measure the `input.capture` budget on `R` and `Dev`.
5. Ask each of Riot, BattlEye and Epic whether a Raw Input/XI2 listener of this kind is acceptable; record their answer (or silence) in the docs.

Keystroke handling rules for the design doc: process key identity only for keys with a binding or a typing-animation class; never write key content to disk, logs, crash dumps or telemetry; no network path from this crate; no synthetic input functions in the crate.

## 8. Open risks

1. **Anti-cheat compatibility is unverified for all three vendors.** No vendor documents a position on Raw Input, XInput or tap listeners. A game could kick or ban a user for Omuzaki. Mitigations in section 3; the real answer is vendor contact (spike 5) and user reports. Top risk.
2. **Wayland has no good global-typing answer.** The sanctioned portal covers hotkeys only; per-key typing needs the `input` group, which weakens the user's security. PF-29 on Wayland is therefore partial or opt-in. Owner should confirm that is acceptable for P0 (this affects the "everything in the spec ships" rule).
3. **Pen position, pressure and tilt unfocused are unverified on Windows, macOS and X11.** Needs the hardware spike. Fallback if it fails: no pen input for PF-29 on that OS, or position only where mouse input proves to carry it.
4. **macOS permission behaviour is flaky in reports:** the prompt may not list the app, taps get disabled, `IOHIDCheckAccess` misreports, GameController background events regressed once. Needs a real Mac (`M` tier is unmeasured).
5. **Windows privilege boundary:** a non-elevated Omuzaki probably can't see an elevated game's input (UIPI), and VSeeFace's docs hint at an admin workaround. Unverified. Asking users to run as admin is a bad answer (a bigger target, more scrutiny); decide before release.
6. **`gilrs` risks:** the default Windows backend silently needs focus; it compiles a second `windows` copy if versions drift; on Linux it needs libudev dev files (system LGPL library, linked dynamically by `libudev-sys`: record under the policy's LGPL-shared rule). Small maintainer base (one GitLab project).
7. **`objc2-game-controller` is a generated binding with ~19k downloads.** Few users have hit its edge cases.
8. **Privacy trust.** One report of "Omuzaki is a keylogger" would cost more than any feature. The indicator, the scope gate and the no-persistence rule are product requirements, not polish.
9. **Hook-based apps set the user's expectation.** Some users will say "other tools use hooks and work". The reply is the design, not a switch to hooks.
10. **Sources are thin in places:** the Apple forum threads were read through search summaries; EAC and Vanguard have no primary technical statement; Warudo docs show little. Revisit when someone can test on real machines.

## 9. Proposed register rows (not added; for the change that builds `omuzaki-input`)

Format as in `docs/licensing/register.md`. The register holds licence verdicts only, so the engineering rejections of `rdev` and `device_query` (both MIT) stay in section 4. Audit column is `(crates.io manifest, 2026-10-12)` until the next audit.

| Item | Kind | Used for | Status | Key condition | Audit |
|---|---|---|---|---|---|
| windows 0.62 (windows-rs) | crate | Raw Input, XInput and HID on Windows (`omuzaki-input`) | yes | MIT OR Apache-2.0: keep the notices; keep the version within gilrs-core's `<=0.62` range | (crates.io: MIT OR Apache-2.0) |
| objc2 0.6, objc2-core-graphics, objc2-game-controller, objc2-io-kit (0.3) | crate | `CGEventTap`, GameController, IOHID fallback on macOS | yes | MIT (objc2); Zlib OR Apache-2.0 OR MIT (framework crates): keep the notices | (crates.io) |
| x11rb 0.14 | crate | XInput2 raw events on Linux X11 | yes | MIT OR Apache-2.0: keep the notices; `xinput` feature, pure-Rust connection | (crates.io) |
| ashpd 0.13 | crate | GlobalShortcuts portal on Linux (PF-05 hotkeys on Wayland) | yes | MIT: keep the notice | (crates.io) |
| evdev 0.13 | crate | opt-in evdev capture on Linux (`input` group) | yes | Apache-2.0 OR MIT: keep the notices; confirm the repo location | (crates.io) |
| gilrs 0.11 / gilrs-core 0.6 | crate | gamepads on all three OSes | conditions | Apache-2.0/MIT: keep the notices; Windows `xinput` feature, no `wgi`; Linux links the system libudev dynamically (LGPL, shared only) | (crates.io; gitlab) |
| SDL_GameControllerDB (via gilrs) | data | gamepad mappings | conditions | Zlib: keep the notice; mappings are crowd-sourced with no per-entry provenance | (repo LICENSE) |
| Microsoft.GameInput (NuGet) | native | possible later Windows gamepad/keyboard backend | unverified | redistribution terms and background behaviour not checked | n/a |
| Wintab (Wacom) | native | possible pen pressure on Windows | unverified | vendor SDK terms not checked; do not adopt without a licence read | n/a |


## Sources not reachable or not read
- Apple Developer Forums threads and the SDL discourse post were read through search summaries only.
- Epic's EOS anti-cheat documentation returned empty; easy.ac support 404'd.
- Riot's support pages redirected; the Riot FAQ page I used is the 2020 news page.
- Microsoft's Windows.Gaming.Input and `GetRawInputBuffer` pages were not read for background-focus statements; the claim about WGI rests on the gilrs README.
