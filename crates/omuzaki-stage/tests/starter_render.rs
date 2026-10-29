//! Starter characters on the GPU: each VRM file in `OMUZAKI_STARTER_VRMS` (a folder) is read, prepared as an avatar the
//! way `io.import {as: avatar}` prepares it, with the `MToon` groups the renderer draws, and rendered through the
//! still path `render.preview` uses, in face, bust and full framings. The pictures go to `OMUZAKI_EVIDENCE_DIR`.
//!
//! The starter models are fetched on demand and never committed (`omuzaki-io-ops`'s `data/starter-characters.toml`),
//! so this test is ignored; run it by hand with both variables set:
//! `cargo test -p omuzaki-stage --features wgpu --test starter_render -- --ignored`.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]
#![cfg(feature = "wgpu")]

use omuzaki_avatar_api::def::{AvatarDef, AvatarSource, SourceFormat};
use omuzaki_core::glam::UVec2;
use omuzaki_io_api::{ImportCtx, ImportInput, ImportOptions, run_import};
use omuzaki_io_ops::prepare::avatar::to_avatar;
use omuzaki_io_vrm::{Vrm0Importer, Vrm1Importer};
use omuzaki_render::{AdapterPreference, HeadlessOptions, WgpuRenderer};
use omuzaki_render_api::{Quality, Renderer};
use omuzaki_stage::body::Body;
use omuzaki_stage::ops::render::job::{DEFAULT_LIGHT_RIG, still_light};
use omuzaki_stage::ops::render::still::{Shot, StillSpec, encode_png, render_body_stills};
use omuzaki_stage::scene::frame::frame;
use omuzaki_stage_api::{CameraDef, Framing};
use omuzaki_store_api::ContentHash;

#[test]
#[ignore = "needs the starter VRMs in OMUZAKI_STARTER_VRMS; they are fetched, never committed"]
#[allow(clippy::disallowed_methods, reason = "reads the fetched models and writes the evidence beside the test")]
fn each_starter_renders_in_face_bust_and_full_shots() {
    let dir = std::env::var_os("OMUZAKI_STARTER_VRMS").expect("set OMUZAKI_STARTER_VRMS to a folder of .vrm files");
    let out = std::env::var_os("OMUZAKI_EVIDENCE_DIR").expect("set OMUZAKI_EVIDENCE_DIR to a folder");
    let mut renderer =
        WgpuRenderer::headless(&HeadlessOptions { adapter: AdapterPreference::Software }).expect("lavapipe opens");
    let mtoon = renderer.capabilities().mtoon;
    let mut files: Vec<_> = std::fs::read_dir(&dir)
        .expect("the folder reads")
        .map(|e| e.expect("an entry").path())
        .filter(|p| p.extension().is_some_and(|x| x == "vrm"))
        .collect();
    files.sort();
    assert!(!files.is_empty(), "no .vrm file in the folder");
    for path in files {
        let name = path.file_stem().expect("a name").to_string_lossy().into_owned();
        let bytes = std::fs::read(&path).expect("reads");
        let options = ImportOptions::default();
        let ctx = ImportCtx::new(&options);
        let (scene, format) = match run_import(&Vrm1Importer, &ImportInput::new(&bytes), &ctx) {
            Ok(done) => (done.scene, SourceFormat::Vrm1),
            Err(_) => (
                run_import(&Vrm0Importer, &ImportInput::new(&bytes), &ctx).expect("a VRM 0.x or 1.0 file").scene,
                SourceFormat::Vrm0,
            ),
        };
        let def = AvatarDef::new(AvatarSource::new(ContentHash::of(&bytes), format, path.to_string_lossy()));
        let avatar = to_avatar(&scene, &def, mtoon).expect("prepares as an avatar").0;
        let body = Body::Avatar(Box::new(avatar));
        let landmarks = body.landmarks().expect("measures");
        let framings = [("face", Framing::Face, 0.0), ("bust", Framing::Bust, 20.0), ("full", Framing::Full, 20.0)];
        let shots: Vec<Shot> = framings
            .iter()
            .map(|(_, framing, yaw)| {
                let mut camera = CameraDef::preset(*framing);
                camera.yaw = *yaw;
                Shot::new(frame(*framing, &landmarks, &camera, 0.75).expect("frames"))
            })
            .collect();
        let spec =
            StillSpec::new(still_light(DEFAULT_LIGHT_RIG).expect("the rig"), UVec2::new(768, 1024), Quality::offline());
        let stills = render_body_stills(&mut renderer, body, &spec, &shots).expect("renders");
        for (still, (label, _, _)) in stills.iter().zip(framings) {
            let png = encode_png(&still.image).expect("encodes");
            std::fs::write(std::path::Path::new(&out).join(format!("{name}-{label}.png")), png).expect("writes");
        }
    }
}
