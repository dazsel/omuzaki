#![cfg(unix)] // the fake install uses a shell script as the stand-in `Update` binary
//! Questions 1 and 5 of the UP-0 verdict, run against the real `velopack` 1.2.161 `UpdateManager`.

use ed25519_dalek::SigningKey;
use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use update_velopack_spike::{feed_json, FeedDouble, SignedSource};
use velopack::locator::VelopackLocatorConfig;
use velopack::{ApplyWaitMode, UpdateCheck, UpdateManager, VelopackAsset};

const NUSPEC: &str = "<?xml version=\"1.0\"?><package><metadata><id>omuzakitest</id><version>VER</version><channel>linux</channel><mainExe>omuzakitest</mainExe></metadata></package>";

/// A `-full.nupkg`: a zip with a `.nuspec` and the AppImage payload (the shape `vpk pack` makes on Linux).
fn nupkg(version: &str, payload: &[u8]) -> Vec<u8> {
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let opts = zip::write::SimpleFileOptions::default();
    zip.start_file("omuzakitest.nuspec", opts).unwrap();
    zip.write_all(NUSPEC.replace("VER", version).as_bytes()).unwrap();
    zip.start_file("omuzakitest.AppImage", opts).unwrap();
    zip.write_all(payload).unwrap();
    zip.finish().unwrap().into_inner()
}

struct Fixture {
    dir: PathBuf,
    cfg: VelopackLocatorConfig,
    host: Arc<FeedDouble>,
    key: SigningKey,
}

impl Fixture {
    fn new(name: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("up0-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(dir.join("packages")).unwrap();
        let update = dir.join("Update");
        // Stand-in for Velopack's native `Update` binary: it only records how it was called.
        fs::write(&update, format!("#!/bin/sh\necho \"$@\" >> {}\n", dir.join("update-calls.log").display())).unwrap();
        fs::set_permissions(&update, fs::Permissions::from_mode(0o755)).unwrap();
        fs::write(dir.join("sq.version"), NUSPEC.replace("VER", "1.0.0")).unwrap();
        fs::write(dir.join("omuzakitest.AppImage"), b"v1.0.0").unwrap();
        let cfg = VelopackLocatorConfig {
            RootAppDir: dir.join("omuzakitest.AppImage"),
            UpdateExePath: update,
            PackagesDir: dir.join("packages"),
            ManifestPath: dir.join("sq.version"),
            CurrentBinaryDir: dir.clone(),
            IsPortable: true,
        };
        Fixture { dir, cfg, host: Arc::default(), key: SigningKey::from_bytes(&[7u8; 32]) }
    }

    fn publish(&self, version: &str, tamper: bool) -> String {
        let file = format!("omuzakitest-{version}-full.nupkg");
        let good = nupkg(version, format!("v{version}").as_bytes());
        let feed = feed_json(&[(&file, version, &good)]);
        let mut served = good.clone();
        if tamper {
            served[good.len() / 2] ^= 0x01; // one flipped byte after signing
        }
        self.host.publish(&self.key, "linux", &feed, &[(&file, &served)]);
        file
    }

    fn manager(&self) -> UpdateManager {
        let source = SignedSource::new(self.host.clone(), self.key.verifying_key());
        UpdateManager::new(source, None, Some(self.cfg.clone())).unwrap()
    }
}

fn available(check: UpdateCheck) -> Box<velopack::UpdateInfo> {
    match check {
        UpdateCheck::UpdateAvailable(i) => i,
        _ => panic!("expected an update"),
    }
}

fn exists(p: &Path) -> bool {
    p.exists()
}

#[test]
fn q5_requests_carry_no_identifier_and_download_is_verified() {
    let fx = Fixture::new("q5");
    let file = fx.publish("1.1.0", false);
    let um = fx.manager();
    let info = available(um.check_for_updates().unwrap());
    um.download_updates(&info, None).unwrap();
    let requests = fx.host.requests();
    println!("recorded requests: {requests:#?}");
    assert_eq!(
        requests,
        ["GET /v1/linux/feed.json", "GET /v1/linux/feed.json.sig", format!("GET /v1/linux/{file}").as_str()]
    );
    for r in &requests {
        for banned in ["localVersion", "id=", "stagingId", "?", "1.0.0"] {
            assert!(!r.contains(banned), "request {r} contains {banned}");
        }
    }
    // The locator still mints a per-install id on disk; our source received it and dropped it.
    assert!(exists(&fx.dir.join("packages/.betaId")));
    let id = fs::read_to_string(fx.dir.join("packages/.betaId")).unwrap();
    assert!(requests.iter().all(|r| !r.contains(&id)));
}

#[test]
fn q5_flipped_byte_is_refused_before_any_file_is_handed_back() {
    let fx = Fixture::new("q5-flip");
    fx.publish("1.1.0", true);
    let um = fx.manager();
    let info = available(um.check_for_updates().unwrap());
    let err = um.download_updates(&info, None).unwrap_err();
    println!("flipped byte -> {err}");
    let left: Vec<_> = fs::read_dir(fx.dir.join("packages")).unwrap().flatten().map(|e| e.file_name()).collect();
    assert!(!left.iter().any(|n| n.to_string_lossy().ends_with(".nupkg")), "nothing handed back: {left:?}");
    assert!(um.get_update_pending_restart().is_none());
}

#[test]
fn q5_tampered_feed_or_signature_is_refused() {
    let fx = Fixture::new("q5-sig");
    fx.publish("1.1.0", false);
    let mut feed = fx.host.get_for_test("/v1/linux/feed.json");
    feed[10] ^= 0x01;
    fx.host.put("/v1/linux/feed.json", feed);
    let err = fx.manager().check_for_updates().err().unwrap();
    println!("tampered feed -> {err}");
    // A feed signed by another key is refused too.
    let other = Fixture::new("q5-sig2");
    other.host.publish(&SigningKey::from_bytes(&[9u8; 32]), "linux", &feed_json(&[]), &[]);
    let um = UpdateManager::new(
        SignedSource::new(other.host.clone(), fx.key.verifying_key()),
        None,
        Some(other.cfg.clone()),
    )
    .unwrap();
    assert!(um.check_for_updates().is_err());
}

#[test]
fn q1_download_never_applies_and_apply_is_a_separate_call_we_can_defer() {
    let fx = Fixture::new("q1");
    let file = fx.publish("1.1.0", false);
    let um = fx.manager();
    let info = available(um.check_for_updates().unwrap());
    um.download_updates(&info, None).unwrap();
    let calls = fx.dir.join("update-calls.log");
    assert!(!exists(&calls), "download must not start the Update binary");
    assert_eq!(fs::read(fx.dir.join("omuzakitest.AppImage")).unwrap(), b"v1.0.0", "installed app untouched");
    let pending: VelopackAsset = um.get_update_pending_restart().expect("downloaded package is pending");
    assert_eq!(pending.FileName, file);
    // "Next start": our launcher decides when, and calls apply without restart.
    um.unsafe_apply_updates(&pending, true, ApplyWaitMode::NoWait, false, Vec::<String>::new()).unwrap();
    let log = (0..50).find_map(|_| {
        fs::read_to_string(&calls).ok().filter(|s| !s.is_empty()).or_else(|| {
            std::thread::sleep(std::time::Duration::from_millis(100));
            None
        })
    });
    let log = log.expect("Update was invoked");
    println!("Update called with: {log}");
    assert!(log.starts_with("apply --package") && log.contains("--norestart") && log.contains("--root"));
}

/// `VelopackApp::run` applies a pending package on startup by default (`app.rs:51-54`, it calls
/// `apply_updates_and_restart_with_args`, which exits the process), so our launcher must turn it off.
#[test]
fn q1_startup_with_auto_apply_disabled_leaves_a_pending_package_alone() {
    let fx = Fixture::new("q1-startup");
    let file = fx.publish("1.1.0", false);
    let um = fx.manager();
    let info = available(um.check_for_updates().unwrap());
    um.download_updates(&info, None).unwrap();
    velopack::VelopackApp::build()
        .set_args(Vec::new())
        .set_auto_apply_on_startup(false)
        .set_locator(fx.cfg.clone())
        .run();
    assert!(!exists(&fx.dir.join("update-calls.log")), "startup must not start the Update binary");
    assert!(exists(&fx.dir.join("packages").join(&file)), "pending package kept for the launcher");
    assert_eq!(fs::read(fx.dir.join("omuzakitest.AppImage")).unwrap(), b"v1.0.0");
}
