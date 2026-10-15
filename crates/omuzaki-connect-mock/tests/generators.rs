//! The generated outputs (#996): exact shapes of textures, GLB, WAV and BVH, media types per manifest, usage equal to
//! the estimate, and seeds that decide the bytes.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]

mod common;

use std::collections::BTreeMap;

use common::{manifest, run};
use omuzaki_connect_api::{
    Artifact, Capability, Connector, ConnectorConfig, ConnectorManifest, DccRequest, MediaKind, Model3dRequest,
    MotionRequest, MotionSource, MusicKind, MusicRequest, Output, Request, Target, TextureRequest, VoiceChoice,
    VoiceRequest,
};
use omuzaki_connect_mock::{FakeConnector, Script};
use serde_json::json;

fn artifact(c: &FakeConnector, id: &str, req: &Request) -> Artifact {
    let out = run(c, &manifest(id), req).result.unwrap();
    match out.outputs.into_iter().next().unwrap() {
        Output::Asset(a) => a,
        other => panic!("{other:?}"),
    }
}

fn texture(tileable: bool, seed: u64) -> Request {
    Request::Texture(TextureRequest {
        prompt: "bricks".into(),
        width: 32,
        height: 16,
        tileable,
        count: 1,
        seed: Some(seed),
        ..TextureRequest::default()
    })
}

fn voice(chars: usize, seed: u64) -> Request {
    Request::Voice(VoiceRequest {
        text: "a".repeat(chars),
        voice: VoiceChoice::preset("kit"),
        language: None,
        seed: Some(seed),
        params: BTreeMap::new(),
    })
}

fn music(seconds: u32, kind: MusicKind, seed: u64) -> Request {
    Request::Music(MusicRequest {
        prompt: "lofi".into(),
        seconds,
        kind,
        count: 1,
        seed: Some(seed),
        ..MusicRequest::default()
    })
}

fn mesh(seed: u64) -> Request {
    Request::Model3d(Model3dRequest { prompt: Some("a cup".into()), seed: Some(seed), ..Model3dRequest::default() })
}

fn motion(seconds: Option<u32>) -> Request {
    Request::Motion(MotionRequest {
        source: MotionSource::prompt("wave"),
        seconds,
        seed: Some(3),
        params: BTreeMap::new(),
    })
}

fn le32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(b[at..at + 4].try_into().unwrap())
}

#[test]
fn tileable_textures_meet_at_their_edges() {
    let tile = artifact(&FakeConnector::texture(Script::default()), "mock-texture", &texture(true, 1));
    let img = image::load_from_memory(&tile.bytes).unwrap().to_rgb8();
    let (w, h) = img.dimensions();
    assert_eq!((w, h), (32, 16));
    assert!((0..h).all(|y| img.get_pixel(0, y) == img.get_pixel(w - 1, y)), "left column = right column");
    assert!((0..w).all(|x| img.get_pixel(x, 0) == img.get_pixel(x, h - 1)), "top row = bottom row");
    let plain = artifact(&FakeConnector::texture(Script::default()), "mock-texture", &texture(false, 1));
    let img = image::load_from_memory(&plain.bytes).unwrap().to_rgb8();
    assert!((0..h).any(|y| img.get_pixel(0, y) != img.get_pixel(w - 1, y)), "a gradient's edges differ");
}

#[test]
fn the_glb_is_valid_and_minimal() {
    let a = artifact(&FakeConnector::model3d(Script::priced(200_000)), "mock-3d", &mesh(1));
    let b = &a.bytes;
    assert_eq!(&b[0..4], b"glTF");
    assert_eq!(le32(b, 4), 2);
    assert_eq!(le32(b, 8) as usize, b.len());
    let json_len = le32(b, 12) as usize;
    assert_eq!(&b[16..20], b"JSON");
    assert_eq!(json_len % 4, 0);
    let gltf: serde_json::Value = serde_json::from_slice(&b[20..20 + json_len]).unwrap();
    assert_eq!(gltf["asset"]["version"], "2.0");
    let bin_at = 20 + json_len;
    assert_eq!((le32(b, bin_at), &b[bin_at + 4..bin_at + 8]), (36, &b"BIN\0"[..]));
    assert_eq!(b.len(), bin_at + 8 + 36);
}

#[test]
fn speech_lasts_a_twentieth_of_a_second_a_character_up_to_ten_seconds() {
    let fake = FakeConnector::voice(Script::priced(30_000));
    for (chars, samples) in [(20, 16_000u32), (400, 160_000)] {
        let a = artifact(&fake, "mock-voice", &voice(chars, 1));
        assert_eq!(&a.bytes[0..4], b"RIFF");
        assert_eq!(le32(&a.bytes, 24), 16_000, "sample rate");
        assert_eq!(le32(&a.bytes, 40), samples * 2, "data length");
        assert_eq!(a.bytes.len(), 44 + samples as usize * 2);
    }
}

#[test]
fn music_is_at_most_two_seconds() {
    let a = artifact(&FakeConnector::music(Script::default()), "mock-music", &music(30, MusicKind::Music, 1));
    assert_eq!((le32(&a.bytes, 24), le32(&a.bytes, 40)), (22_050, 44_100 * 2));
    let sfx = artifact(&FakeConnector::music(Script::default()), "mock-music", &music(1, MusicKind::Sfx, 1));
    assert_eq!(le32(&sfx.bytes, 40), 22_050 * 2);
    assert_ne!(
        sfx.bytes,
        artifact(&FakeConnector::music(Script::default()), "mock-music", &music(1, MusicKind::Music, 1)).bytes
    );
}

#[test]
fn bvh_frames_follow_the_seconds() {
    for (seconds, frames) in [(Some(5), 60), (None, 30)] {
        let a = artifact(&FakeConnector::motion(Script::default()), "mock-motion", &motion(seconds));
        let text = String::from_utf8(a.bytes).unwrap();
        assert!(text.starts_with("HIERARCHY\nROOT Hips") && text.contains("JOINT Spine"));
        assert!(text.contains(&format!("MOTION\nFrames: {frames}\n")), "{seconds:?}");
        let rows = text.lines().skip_while(|l| !l.starts_with("Frame Time")).skip(1).count();
        assert_eq!(rows, frames);
        assert_eq!(a.name.as_deref(), Some("motion.bvh"));
    }
}

#[test]
fn dcc_echoes_its_call() {
    let req = Request::Dcc(DccRequest {
        tool: "mock-dcc".into(),
        action: "export".into(),
        asset: None,
        args: BTreeMap::from([("scale".to_owned(), json!(2))]),
    });
    let out = run(&FakeConnector::dcc(Script::default()), &manifest("mock-dcc"), &req).result.unwrap();
    assert_eq!(out.outputs, [Output::Json(json!({"tool": "mock-dcc", "action": "export", "echo": {"scale": 2}}))]);
}

/// A fake, its registered id, a request, and a request for a given seed.
type Case = (FakeConnector, &'static str, Request, fn(u64) -> Request);

/// The cases for each byte-making fake, priced as its manifest says.
fn cases() -> Vec<Case> {
    vec![
        (FakeConnector::texture(Script::default()), "mock-texture", texture(false, 1), |s| texture(false, s)),
        (FakeConnector::model3d(Script::priced(200_000)), "mock-3d", mesh(1), mesh),
        (FakeConnector::voice(Script::priced(30_000)), "mock-voice", voice(30, 1), |s| voice(30, s)),
        (FakeConnector::music(Script::default()), "mock-music", music(1, MusicKind::Music, 1), |s| {
            music(1, MusicKind::Music, s)
        }),
        (FakeConnector::motion(Script::default()), "mock-motion", motion(Some(1)), |_| motion(Some(1))),
    ]
}

fn media_kind(media_type: &str) -> MediaKind {
    match media_type {
        "image/png" => MediaKind::Image,
        "model/gltf-binary" => MediaKind::Mesh,
        "audio/wav" => MediaKind::Audio,
        "text/plain" => MediaKind::Motion,
        other => panic!("{other}"),
    }
}

#[test]
fn outputs_fit_their_manifest_and_cost_their_estimate() {
    for (fake, id, req, _) in cases() {
        let m: ConnectorManifest = manifest(id);
        let a = artifact(&fake, id, &req);
        let decl = m.declares(req.capability()).unwrap();
        assert!(decl.outputs.contains(&a.kind) && media_kind(&a.media_type) == a.kind, "{id}");
        assert!(!a.model.is_empty(), "{id}");
        let target = Target::from_manifest(&m, &ConnectorConfig::default()).unwrap();
        let estimate = fake.estimate(&m, &target, &req).unwrap();
        assert_eq!(run(&fake, &m, &req).result.unwrap().usage.usd_micros, estimate.usd_micros, "{id}");
    }
}

#[test]
fn seeds_decide_the_bytes() {
    for (fake, id, _, make) in cases() {
        if make(1).capability() == Capability::Motion {
            continue;
        }
        let (a, again, b) =
            (artifact(&fake, id, &make(1)), artifact(&fake, id, &make(1)), artifact(&fake, id, &make(2)));
        assert_eq!(a.bytes, again.bytes, "{id}: equal seeds, equal bytes");
        assert_ne!(a.bytes, b.bytes, "{id}: different seeds, different bytes");
    }
}

#[test]
fn all_six_manifests_validate() {
    for id in ["mock-texture", "mock-3d", "mock-voice", "mock-music", "mock-motion", "mock-dcc"] {
        assert_eq!(manifest(id).validate(), Ok(()), "{id}");
    }
    for id in ["mock-3d", "mock-voice"] {
        assert_eq!(manifest(id).cost.priced(), Some("2026-10-13"));
    }
}
