# Pen position, pressure and tilt while unfocused (2026-10-13)

> Research pass (PF-29 spike), a follow-up to `2026-10-os-input-capture.md`. **Nothing here was run on a pen.** The research environment has no tablet, no Windows, no macOS and no X server with a digitiser, so every row is from documentation or protocol text, and the "Verdict" column says how far to trust it. The hardware part of the acceptance (device and OS build per cell) is **not done**; section 4 is the protocol that closes it, and section 5 lists what is still owed.

## 1. Summary

Verdict legend: **documented** = a primary source (spec, header, protocol text) says it; **inferred** = follows from documented parts but no source states it for this case; **unknown** = no source, hardware needed.

| | Position | Pressure | Tilt | Verdict | Recommended path |
|---|---|---|---|---|---|
| **Windows** | partial | unknown | unknown | position **inferred**, pressure and tilt **unknown** | Mouse Raw Input (`RIDEV_INPUTSINK`) for position. Try Raw Input HID registration for usage page 0x0D for pressure and tilt (spike). |
| **macOS** | yes | yes | yes | field definitions **recalled** from the SDK header; background delivery to a tap **inferred**; not run | Listen-only `CGEventTap` reading the `kCGTabletEvent*` fields on mouse and tablet events. `IOHIDManager` only if the tap lacks a field. |
| **Linux X11** | yes | yes | yes | raw-event delivery regardless of focus **documented** (XI2 protocol); tablet valuators in raw events **inferred**; not run | XI2 raw events on the root window, selected for `XIAllDevices`; read pressure and tilt valuators by label. |
| **Wayland** | no passive path | | | as in the input-capture note | opt-in evdev only |

The three OSes do not behave alike. Windows is the least certain, but macOS and X11 are only inferred too: nothing here was run. The PF-29 default, conditional on the spikes in section 4: macOS and X11 take pen axes from the same event stream as keyboard and mouse; Windows reports pen input unavailable unless a spike shows the mouse path (position) or the HID path (pressure, tilt) delivers.

## 2. Per OS

### 2.1 Windows

- **Pen as a HID device.** Windows lists digitiser pens (usage page 0x0D, usage 0x01 digitizer / 0x02 pen) as top-level collections that the Raw Input Manager opens ([HID architecture](https://learn.microsoft.com/en-us/windows-hardware/drivers/hid/hid-architecture)). That document is the source for the claim that a pen is a HID device. It does not say Raw Input delivers its reports to an application.
- **Pointer stack takes the pen.** Since Windows 8 the pen is routed through the pointer input stack (`WM_POINTER*`), which delivers to the window under the pen, or to the capture window while in contact ([WM_POINTERUPDATE](https://learn.microsoft.com/en-us/windows/win32/inputmsg/wm-pointerupdate)). That is no use unfocused. The same page says unhandled pointer input "may generate mouse input", which is the only documented route to a mouse-shaped pen position.
- **Raw Input registration.** `RegisterRawInputDevices` accepts any usage page and usage, and `RIDEV_INPUTSINK` asks for delivery while in the background ([RAWINPUTDEVICE](https://learn.microsoft.com/en-us/windows/win32/api/winuser/ns-winuser-rawinputdevice)). Nothing there says the digitiser page is excluded, and nothing says it is delivered. **Unknown.** Two things could stop it: the pointer stack may own the device so `WM_INPUT` never fires, or the HID report may carry a descriptor only the pen driver parses.
- **Position through the mouse path: inferred.** If the pen drives the system cursor (Windows synthesises mouse messages for pens the foreground app ignores), mouse Raw Input with `RIDEV_INPUTSINK` would see relative or absolute motion. Absolute devices are flagged `MOUSE_MOVE_ABSOLUTE`, in a 0..65535 range (RAWMOUSE docs). Whether a synthesised mouse message reaches Raw Input at all is the unknown; Raw Input is below the cursor, so it may not.
- **Wintab** (Wacom driver API) gives pressure and tilt but is a vendor API with its own terms and is not evaluated here (as in the input-capture note). Do not adopt without a licence read.
- **Question to settle on hardware:** register `{0x0D, 0x02}` and `{0x0D, 0x01}` with `RIDEV_INPUTSINK` on a hidden window, with another app focused, and log whether `WM_INPUT` arrives with `RIM_TYPEHID`. If it does, parse with `HidP_GetCaps`, `HidP_GetValueCaps` and `HidP_GetUsageValue` from `GetRawInputDeviceInfo(RIDI_PREPARSEDDATA)` (usages Tip Pressure 0x30, X Tilt 0x3D, Y Tilt 0x3E on page 0x0D).

### 2.2 macOS

- **The event model carries pen axes on ordinary events.** CoreGraphics defines tablet fields on `CGEventField`: `kCGTabletEventPointX/Y/Z`, `kCGTabletEventPointButtons`, `kCGTabletEventPointPressure`, `kCGTabletEventTiltX`, `kCGTabletEventTiltY`, `kCGTabletEventRotation`, `kCGTabletEventTangentialPressure`, `kCGTabletEventDeviceID` (header `CGEventTypes.h`; also the event types `kCGEventTabletPointer` and `kCGEventTabletProximity`). Mouse events from a tablet carry these fields too. This is from my knowledge of the header, not a fetched page: re-check names against the SDK before building.
- **Listen-only tap sees them.** A tap observes the same `CGEvent`s that are posted to apps, whatever the focus. So pressure and tilt reach a listen-only `CGEventTap` if the event mask includes mouse move, drag, down, up and the two tablet types. That a tap sees every posted event is Apple's stated purpose for taps, but I found no source stating that tablet events or their fields reach a listen-only tap while another app is focused: **inferred, not tested**, and field definitions only show how a value is represented, not that it is delivered. It needs the Input Monitoring permission already planned in the input-capture note.
- **Tablet driver caveat.** Wacom and Huion drivers post these events themselves, so the fields depend on the driver being installed. Without a vendor driver, macOS generic HID tablets may deliver position only. Unknown; hardware.
- **`IOHIDManager`** is the fallback if the tap lacks a field: same permission, raw HID reports to parse (usage page 0x0D). Not needed unless the tap fails.

### 2.3 Linux X11

- **Raw events and valuators.** XI2's `XIRawEvent` carries `valuators` (a mask) and `raw_values`; raw events go to root windows and, with XI 2.1+, arrive "even when the device is grabbed by another client" ([XI2proto](https://xorg.freedesktop.org/archive/current/doc/inputproto/XI2proto.txt)). So focus does not matter, and a tablet's pressure and tilt valuators are in the raw event if the device exposes them.
- **Select on `XIAllDevices`, not `XIAllMasterDevices`.** Raw events for a master device report its attached slave's valuators only for the axes the master exposes; a tablet's pressure and tilt live on the slave device (the "stylus" device that `xinput list` shows). Selecting `XIAllDevices` (or the tablet slave's id) is the safe route. Inferred from the protocol text on master and slave valuators; verify.
- **Find axes by label, not index.** Use `XIQueryDevice` to list valuator classes and match the atoms `Abs Pressure`, `Abs Tilt X`, `Abs Tilt Y`, `Abs X`, `Abs Y` (the wacom and libinput drivers use these labels). Ranges (min and max) come from the same class, so normalisation needs no per-vendor table.
- **Raw values are unaccelerated and unclipped:** that is right for pressure and tilt, and means position from raw values is device units, not screen pixels. For an arm-follows-pen effect use the device range from `XIQueryDevice` to normalise.
- **Binding:** `x11rb` with `xinput`, as in the input-capture note. No new dependency.

### 2.4 Wayland

No passive path, as in the input-capture note: evdev opt-in. A tablet is an evdev device with `ABS_PRESSURE`, `ABS_TILT_X`, `ABS_TILT_Y` (kernel `input-event-codes.h`), so evdev gives all three axes once the user has granted `input` group access, with the same security caveat as typing.

## 3. Recommended path for PF-29

1. Add a `PenSample { position: [f32; 2], pressure: Option<f32>, tilt: Option<[f32; 2]>, in_range: bool }` to the normalised event type in the proposed `omuzaki-input`. Each backend fills what it can and leaves `None` otherwise, so the UI can say "no pressure on this system".
2. macOS and X11: pen axes come from the existing capture stream; no second backend.
3. Windows: position support is conditional on the mouse-path spike (does a synthesised mouse message reach Raw Input?); pressure and tilt on the HID spike 4.1 (`WM_INPUT` delivery). If neither delivers, pen input is reported unavailable on Windows, and the Wintab licence read becomes the next task.
4. Coalesce pen motion to the avatar frame rate on the capture thread (digitisers run at 133 to 360 Hz, comparable to a 1000 Hz mouse).
5. Same privacy rules as the input-capture note: in memory only, scope-gated, indicator shown.

## 4. Hardware protocol (what a person with the hardware runs)

Each test is a small program that logs timestamp, device id, x, y, pressure, tilt while another window (a text editor) has focus and the pen draws in a third app. Record device, driver version and OS build in the results table in section 5.

### 4.1 Windows (Wacom and a Surface-class pen)
Rust with the `windows` crate (already proposed in the input-capture note). Hidden window; `RegisterRawInputDevices` for (a) `{0x01,0x02}` mouse and (b) `{0x0D,0x01}` and `{0x0D,0x02}`, all `RIDEV_INPUTSINK`; `WM_INPUT` handler logs `dwType` and, for HID, the report bytes. Cases to run: Wacom with and without its driver; Surface Pen on Windows Ink; Ink on and off in Settings; the focused app being elevated; the pen hovering (proximity) and in contact. Answer per axis: yes, no or partial.

### 4.2 macOS (a Wacom tablet, plus a built-in trackpad as a control)
Rust with `objc2-core-graphics` (proposed in the input-capture note): listen-only `CGEventTapCreate` for mouse and tablet event types, log the `kCGTabletEvent*` fields on each event. Run with a vendor driver and without. If any field is zero while the pen moves, repeat with `IOHIDManager` on usage page 0x0D.

### 4.3 Linux X11 (a Wacom or any libinput tablet)
Rust with `x11rb`: select `XI_RawMotion`, `XI_RawButtonPress` for `XIAllDevices` on the root; on each raw event decode the valuator mask and `raw_values` and print them with the axis labels from `XIQueryDevice`. Cross-check with `xinput test-xi2 --root`.

## 5. Results (to fill)

| OS | Device | Driver / OS build | Position | Pressure | Tilt | Notes |
|---|---|---|---|---|---|---|
| Windows | Wacom (model) | | | | | |
| Windows | Surface-class pen | | | | | |
| macOS | Wacom (model) | | | | | |
| Linux X11 | tablet (model) | | | | | |

**Not yet done:** every cell above. This note settles the design and the test protocol only; it does not meet the acceptance check of "yes/no/partial per OS and axis, with the device and OS build". The spike stays open until the table is filled.

## 6. Risks

1. Windows pen unfocused may be position-only or nothing; the pointer stack owning the pen is the likely blocker.
2. macOS tablet fields depend on a vendor driver for non-Apple tablets.
3. A synthesised mouse message may not reach Raw Input, which would remove even the position fallback on Windows.
4. Sources: the CoreGraphics field names come from memory of the SDK header, and the X11 master/slave point from protocol reading; both are cheap to confirm on real hardware.
