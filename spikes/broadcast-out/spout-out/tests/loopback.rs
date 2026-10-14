//! The whole Spout path end to end, in two processes: the sender renders with wgpu on DX12 and hands frames to Spout, a
//! separate receiver process reads them back and checks them. Windows only; skips (with a note) when there is no GPU.
//!
//! Run from WSL with `cargo test --target x86_64-pc-windows-gnu`: the Windows test exe starts through interop.

#![cfg(windows)]
#![allow(clippy::unwrap_used)] // tests may unwrap: a panic is a clear failure

use std::process::Command;

use spout_out::analysis::{Blend, Sent};
use spout_out::report::LoopbackReport;

/// Runs `spout-out loopback` with `extra` arguments; `None` means there was no GPU to run on.
fn loopback(extra: &[&str]) -> Option<LoopbackReport> {
    let output = Command::new(env!("CARGO_BIN_EXE_spout-out"))
        .args(["loopback", "--json", "--frames", "120", "--fps", "60", "--size", "1280x720", "--hash-every", "20"])
        .args(extra)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !output.status.success() {
        assert!(stderr.contains("no suitable GPU adapter"), "loopback failed: {stderr}");
        eprintln!("skipping: no GPU adapter");
        return None;
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let line = stdout.lines().find(|l| l.starts_with('{')).unwrap();
    Some(serde_json::from_str(line).unwrap())
}

fn assert_delivered_whole(report: &LoopbackReport) {
    let (sent, got) = (&report.sender, &report.receiver);
    assert_eq!(got.torn, 0, "a frame arrived torn");
    assert_eq!(got.patch_failures, 0, "a frame arrived with wrong patch bytes");
    // An event-driven receiver can wake twice for one frame (the event is signalled per frame, the read takes the newest).
    assert!(got.repeats <= 3, "{} repeated reads", got.repeats);
    // The receiver may miss a frame or two if the OS deschedules it; a systematic loss would be a bug.
    assert!(got.received + 5 >= sent.sent, "received {} of {}", got.received, sent.sent);
    assert!(sent.patches_ok);
    let compared: Vec<_> =
        sent.hashes.iter().filter_map(|(f, h)| got.hashes.iter().find(|(g, _)| g == f).map(|(_, t)| (h, t))).collect();
    assert!(!compared.is_empty(), "no sampled frame reached both sides");
    assert!(compared.iter().all(|(ours, theirs)| ours == theirs), "pixels changed on the way: {compared:?}");
}

#[test]
fn gpu_path_with_fence_delivers_every_frame_bit_for_bit() {
    let Some(report) = loopback(&["--path", "gpu", "--sync", "fence"]) else { return };
    assert_delivered_whole(&report);
}

#[test]
fn cpu_path_with_wait_delivers_every_frame_bit_for_bit() {
    let Some(report) = loopback(&["--path", "cpu", "--sync", "fence"]) else { return };
    assert_delivered_whole(&report);
}

#[test]
fn premultiplied_frames_need_a_premultiplied_blend() {
    let Some(report) = loopback(&["--path", "gpu", "--sync", "fence"]) else { return };
    let error = |sent, blend| {
        report.sender.composite.iter().find(|c| c.sent == sent && c.blend == blend).map(|c| c.max_error).unwrap()
    };
    // Right pairings reproduce the ground truth to within rounding.
    assert!(error(Sent::Premultiplied, Blend::Premultiplied) < 2.0);
    assert!(error(Sent::Premultiplied, Blend::UnpremultiplyThenStraight) < 2.0);
    assert!(error(Sent::Straight, Blend::Straight) < 2.0);
    // Wrong pairings are visibly wrong: a dark halo one way, a bright fringe the other.
    assert!(error(Sent::Premultiplied, Blend::Straight) > 30.0);
    assert!(error(Sent::Straight, Blend::Premultiplied) > 100.0);
}

#[test]
fn an_obs_style_receiver_written_from_the_protocol_reads_the_shared_texture() {
    // Reads the sender's SharedTextureInfo from shared memory and opens the legacy share handle itself, as OBS's Spout2
    // source does, without the SDK's receiver class. A frame it happens to read mid-copy may be torn, so this checks that
    // the texture opens and carries the right pixels, not that every read is whole (that is what the benchmark measures).
    let Some(report) = loopback(&["--path", "gpu", "--sync", "fence", "--receiver", "raw", "--poll-hz", "240"]) else {
        return;
    };
    let (sent, got) = (&report.sender, &report.receiver);
    assert!(got.received + 20 >= sent.sent, "read {} of {} frames", got.received, sent.sent);
    assert_eq!(got.patch_failures, 0);
    let matching = sent.hashes.iter().filter(|(f, h)| got.hashes.iter().any(|(g, t)| g == f && t == h)).count();
    assert!(matching > 0, "no sampled frame matched the sender's: {:?} vs {:?}", sent.hashes, got.hashes);
}
