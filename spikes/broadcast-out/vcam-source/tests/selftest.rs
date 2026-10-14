//! The media source through Media Foundation, in this process, with no installation: `vcam-probe selftest` loads the DLL by
//! path (no COM registration), makes the source from its class factory and reads it as an app would.
//!
//! Windows only. Run from WSL: `cargo build -p vcam-source --target x86_64-pc-windows-gnu`, then the same with `test` (the
//! Windows test exe starts through interop).

#![cfg(windows)]
#![allow(clippy::unwrap_used)] // tests may unwrap: a panic is a clear failure

use std::path::Path;
use std::process::Command;

/// The camera DLL must sit next to the probe. `cargo test` builds only the rlib of a package that is also a cdylib, and
/// the Windows test exe cannot run the Linux cargo that started it, so the DLL is built first with
/// `cargo build -p vcam-source --target x86_64-pc-windows-gnu` (the same profile as the test).
fn require_dll(probe: &Path) {
    let dll = probe.parent().unwrap().join("omuzaki_vcam_source.dll");
    assert!(
        dll.exists(),
        "build the DLL first: cargo build -p vcam-source --target x86_64-pc-windows-gnu ({} is missing)",
        dll.display()
    );
}

#[test]
fn the_source_works_through_the_media_foundation_source_reader() {
    let probe = Path::new(env!("CARGO_BIN_EXE_vcam-probe"));
    require_dll(probe);
    let output = Command::new(probe).args(["selftest", "--frames", "45"]).output().unwrap();
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(output.status.success(), "selftest failed:\n{text}\n{}", String::from_utf8_lossy(&output.stderr));

    assert!(text.contains("unknown CLSID refused: true"), "{text}");
    assert!(text.contains("(true, true, true)"), "required interfaces missing:\n{text}");
    assert!(text.contains("characteristics: 0x1"), "{text}");
    for format in ["NV12", "YUY2"] {
        let line = text
            .lines()
            .find(|l| l.starts_with(&format!("{format}:")))
            .unwrap_or_else(|| panic!("no {format} line:\n{text}"));
        assert!(line.contains("wrong sized buffers 0"), "{line}");
        assert!(line.contains("frame-number gaps 0"), "{line}");
        assert!(line.contains("timestamp irregularities 0"), "{line}");
    }
    let rgb = text.lines().find(|l| l.starts_with("RGB32")).unwrap();
    let error: u32 = rgb.split("error ").nth(1).and_then(|rest| rest.split(' ').next()).unwrap().parse().unwrap();
    assert!(error <= 3, "bar colours off by {error} levels: {rgb}");
    assert!(text.contains("DllCanUnloadNow after releasing everything: true"), "objects leaked:\n{text}");
}

#[test]
fn registering_is_described_never_done() {
    let probe = Path::new(env!("CARGO_BIN_EXE_vcam-probe"));
    let output = Command::new(probe).args(["print-registration"]).output().unwrap();
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.contains("HKLM") && text.contains("InprocServer32"));
    // start-camera refuses without the explicit confirmation flag.
    let refused = Command::new(probe).args(["start-camera"]).output().unwrap();
    assert!(!refused.status.success());
}
