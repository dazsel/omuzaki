//! Frames for scoring the body tracker (PF-06, PF-07) against a known answer: the kit's own character, posed in each
//! pose of the pose library and in a sweep of arm poses, rendered front-on at a webcam's framing (head to just
//! below the hips, 1280x720, the webcam's default mode), each written as a binary PPM beside a JSON of the pose it shows. `omuzaki-test-track`'s `track_body`
//! reads them, tracks them and scores the solved arms and fingers against the JSON.
//!
//! Gated on `OMUZAKI_BODY_FRAMES_DIR`; on lavapipe with the `wgpu` feature, through the render mock without it (whose
//! pictures are flat, so only the plumbing is exercised).
//!
//! **The pixel loop.** With `OMUZAKI_BODY_SOLVED_DIR` too (what `track_body` solved for each frame), the same character
//! is driven by what the tracker saw, through the stage's own tracked-body layer, re-rendered from the same camera in
//! the same light, and diffed against the frame pixel by pixel. The tracker matches a frame when its map is 0. Each
//! frame's truth is re-rendered first as the control, so a non-zero map is the tracker's and never the renderer's. With
//! `OMUZAKI_BODY_PARITY_DIR`, each frame's map and overlay are written there as PPMs; keep that folder outside the
//! repository.
//!
//! **The fit.** With `OMUZAKI_BODY_FIT` as well (a comma-separated list of frames, `all`, or `control` to check only that
//! a frame's own pose redrawn gives the frame back), each frame is fitted by
//! rendering and comparing (`omuzaki_track::fit`), starting from what the tracker solved: the character is redrawn on
//! one live stage per candidate pose, exactly as a still is, and the pose walks toward the one whose render matches the
//! frame. It prints each frame's map before and after, and writes the fitted render and its map.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::expect_used, clippy::unwrap_used)]
#![allow(clippy::cast_precision_loss, clippy::cast_possible_truncation, clippy::cast_sign_loss)]

use omuzaki_anim::poses::PoseLibrary;
use omuzaki_anim_api::{ChannelBuf, HumanoidBone, HumanoidPose, Pose, Tick};
use omuzaki_char::builder::CachingBuilder;
use omuzaki_char_api::{BuildOptions, BuildServices, BuiltCharacter, CharId, CharacterBuilder, CharacterDef};
use omuzaki_core::glam::{Quat, UVec2, Vec3};
use omuzaki_core::time::FakeClock;
use omuzaki_doc_api::{DocPath, DocRead, DocWrite};
use omuzaki_doc_mock::MemDoc;
use omuzaki_ops_api::{Actor, OpCtx, find};
use omuzaki_render_api::{AlphaEncoding, Camera, Projection, Quality, Renderer, Scene, TargetDesc};
use omuzaki_stage::UploadOptions;
use omuzaki_stage::frame::face::{FaceRig, SETTLE_STEPS};
use omuzaki_stage::frame::{FrameInput, Stage, StageClock};
use omuzaki_stage::ops::render::pose::PoseArg;
use omuzaki_stage::ops::render::still::{Shot, StillLight, StillSpec, render_stills};
use omuzaki_stage::perform::body::TrackedBody;
use omuzaki_stage::recipe::LightRig;
use omuzaki_track::fit::{FitConfig, Level, Synth, Targets, fit};
use omuzaki_track_api::body::{BodyFrame, BodySource, Fingers};
use serde_json::{Value, json};

/// The bones the score reads: the upper chest, both arms and every finger.
const BONES: [HumanoidBone; 37] = {
    use HumanoidBone as B;
    [
        B::UpperChest,
        B::LeftUpperArm,
        B::LeftLowerArm,
        B::LeftHand,
        B::RightUpperArm,
        B::RightLowerArm,
        B::RightHand,
        B::LeftThumbMetacarpal,
        B::LeftThumbProximal,
        B::LeftThumbDistal,
        B::LeftIndexProximal,
        B::LeftIndexIntermediate,
        B::LeftIndexDistal,
        B::LeftMiddleProximal,
        B::LeftMiddleIntermediate,
        B::LeftMiddleDistal,
        B::LeftRingProximal,
        B::LeftRingIntermediate,
        B::LeftRingDistal,
        B::LeftLittleProximal,
        B::LeftLittleIntermediate,
        B::LeftLittleDistal,
        B::RightThumbMetacarpal,
        B::RightThumbProximal,
        B::RightThumbDistal,
        B::RightIndexProximal,
        B::RightIndexIntermediate,
        B::RightIndexDistal,
        B::RightMiddleProximal,
        B::RightMiddleIntermediate,
        B::RightMiddleDistal,
        B::RightRingProximal,
        B::RightRingIntermediate,
        B::RightRingDistal,
        B::RightLittleProximal,
        B::RightLittleIntermediate,
        B::RightLittleDistal,
    ]
};

/// The default character of the standard template, created through `character.create`.
fn kit_def() -> CharacterDef {
    omuzaki_stage::link();
    let mut doc = MemDoc::new();
    let registration = find("character.create").expect("character.create is registered");
    let op = (registration.decode)(json!({"id": "kit", "name": "Kit", "template": "standard", "seed": 7}))
        .expect("the arguments decode");
    let (actor, clock) = (Actor::Person { name: "tester".into() }, FakeClock::default());
    let plan = op.plan(&OpCtx::new(&doc, &actor, &clock)).expect("the character is created");
    doc.apply(&plan.patch).expect("the patch applies");
    let node = doc.get(&DocPath::parse("characters/kit").unwrap()).expect("stored").clone();
    CharacterDef::from_node(&node).expect("reads back")
}

/// The character built as the app builds it, by `builder` (which caches, so a rebuild per still is cheap).
fn build(builder: &CachingBuilder, def: &CharacterDef) -> BuiltCharacter {
    builder
        .build(&CharId::new("kit").unwrap(), def, &BuildOptions::default(), &BuildServices::default())
        .expect("the default character builds")
}

/// A pose that turns the arms by `upper` and `lower` (T space, the left arm; the right arm mirrored).
fn arms(upper: Quat, lower: Quat) -> HumanoidPose {
    let mut pose = HumanoidPose::REST;
    pose.set(HumanoidBone::LeftUpperArm, upper);
    pose.set(HumanoidBone::LeftLowerArm, lower);
    let mirrored = pose.mirrored();
    pose.set(HumanoidBone::RightUpperArm, mirrored.get(HumanoidBone::RightUpperArm));
    pose.set(HumanoidBone::RightLowerArm, mirrored.get(HumanoidBone::RightLowerArm));
    pose
}

/// The poses to score: every library pose, then a sweep of the arms.
fn poses(built: &BuiltCharacter) -> Vec<(String, HumanoidPose)> {
    let library = PoseLibrary::builtin().expect("the pose library loads");
    let mut out: Vec<(String, HumanoidPose)> = library
        .iter()
        .filter_map(|def| {
            let pose = PoseArg::new(def.id.clone()).resolve_on("pose", &built.rig.skeleton).ok()?;
            Some((def.id.clone(), pose))
        })
        .collect();
    let deg = f32::to_radians;
    // Arms down at 60 degrees, then elbows bent forward by steps, then arms raised.
    for (name, down, bend) in [
        ("arms-a-pose", -60.0, 0.0),
        ("arms-down-bend-45", -70.0, 45.0),
        ("arms-down-bend-90", -70.0, 90.0),
        ("arms-down-bend-120", -70.0, 120.0),
        ("arms-level-bend-90", 0.0, 90.0),
        ("arms-raised-30", 30.0, 20.0),
    ] {
        // Down is a turn about +Z for the left arm (its +X toward -Y); the elbow bends the forearm toward +Z.
        let upper = Quat::from_rotation_z(deg(down));
        let lower = Quat::from_rotation_y(-deg(bend));
        out.push((name.to_owned(), arms(upper, lower)));
    }
    out
}

/// A webcam's view of the character: front-on, from just above the head to a little below the hips (where hands
/// hanging at the sides reach), 16:9.
fn shot(built: &BuiltCharacter) -> Shot {
    let skeleton = &built.rig.skeleton;
    let mut world = Pose::IDENTITY;
    skeleton.world_pose(&skeleton.rest_pose(), &mut world);
    let joint = |bone| world.local[skeleton.humanoid_bone(bone).expect("a humanoid bone").index()].translation;
    let hips = joint(HumanoidBone::Hips);
    // The top of the hair, from the meshes; the bottom 20 cm below the hips, where hands hanging at the sides reach.
    let hair = built.meshes.iter().flat_map(|m| m.mesh.geometry.positions.iter().map(|p| p.y)).fold(f32::MIN, f32::max);
    let (top, bottom) = (hair + 0.03, hips.y - 0.2);
    let centre = Vec3::new(hips.x, f32::midpoint(top, bottom), hips.z);
    let fov_y_deg = 45.0_f32;
    let distance = 0.5 * (top - bottom) / (0.5 * fov_y_deg.to_radians()).tan();
    Shot::new(Camera::look_at(
        centre + Vec3::new(0.0, 0.0, distance),
        centre,
        Vec3::Y,
        Projection::Perspective { fov_y_deg, near: 0.05, far: 20.0 },
    ))
}

/// The picture as a binary PPM (RGB, alpha dropped over the background the still already has).
fn ppm(image: &omuzaki_render_api::Image) -> Vec<u8> {
    let mut out = format!("P6\n{} {}\n255\n", image.size.x, image.size.y).into_bytes();
    for pixel in image.rgba8.as_chunks::<4>().0 {
        out.extend_from_slice(&pixel[..3]);
    }
    out
}

fn truth(pose: &HumanoidPose) -> Value {
    let bones: serde_json::Map<String, Value> =
        BONES.iter().map(|&bone| (format!("{bone:?}"), json!(pose.get(bone).to_array()))).collect();
    json!({ "bones": bones })
}

/// The software renderer on lavapipe, or the mock without the `wgpu` feature.
fn renderer() -> Box<dyn Renderer> {
    #[cfg(feature = "wgpu")]
    {
        use omuzaki_render::{AdapterPreference, HeadlessOptions, WgpuRenderer};
        Box::new(
            WgpuRenderer::headless(&HeadlessOptions { adapter: AdapterPreference::Software })
                .expect("a software adapter (install mesa-vulkan-drivers)"),
        )
    }
    #[cfg(not(feature = "wgpu"))]
    Box::new(omuzaki_render_mock::MockRenderer::new())
}

/// The light every frame is rendered in.
fn light() -> StillLight {
    let rig = LightRig::load("viewer-studio").unwrap();
    StillLight::new(rig.to_lights().unwrap(), rig.background(), rig.post(false))
}

/// `pose` on the built character, from `shot`, as RGBA.
fn render(
    renderer: &mut dyn Renderer,
    built: BuiltCharacter,
    light: &StillLight,
    shot: &Shot,
    pose: &HumanoidPose,
) -> omuzaki_render_api::Image {
    let spec = StillSpec::new(light.clone(), UVec2::new(1280, 720), Quality::offline()).with_pose(Some(*pose));
    render_stills(renderer, built, &spec, std::slice::from_ref(shot)).unwrap().remove(0).image
}

#[test]
#[allow(clippy::disallowed_methods, reason = "test evidence, written to a folder the developer names; not an op")]
fn write_the_body_tracking_frames_when_asked() {
    let Some(directory) = std::env::var_os("OMUZAKI_BODY_FRAMES_DIR").map(std::path::PathBuf::from) else {
        eprintln!("skipped: set OMUZAKI_BODY_FRAMES_DIR to write the frames");
        return;
    };
    std::fs::create_dir_all(&directory).unwrap();
    let mut renderer = renderer();
    let (builder, def) = (CachingBuilder::new(), kit_def());
    let built = build(&builder, &def);
    let (light, shot) = (light(), shot(&built));
    for (name, pose) in poses(&built) {
        let image = render(renderer.as_mut(), build(&builder, &def), &light, &shot, &pose);
        std::fs::write(directory.join(format!("{name}.ppm")), ppm(&image)).unwrap();
        std::fs::write(directory.join(format!("{name}.json")), truth(&pose).to_string()).unwrap();
    }
}

/// A binary PPM's size and RGB bytes.
fn read_ppm(bytes: &[u8]) -> (u32, u32, &[u8]) {
    let mut fields = Vec::new();
    let mut at = 0;
    while fields.len() < 4 {
        while bytes[at].is_ascii_whitespace() {
            at += 1;
        }
        let start = at;
        while !bytes[at].is_ascii_whitespace() {
            at += 1;
        }
        fields.push(std::str::from_utf8(&bytes[start..at]).unwrap().to_owned());
    }
    assert_eq!(fields[0], "P6");
    (fields[1].parse().unwrap(), fields[2].parse().unwrap(), &bytes[at + 1..])
}

/// A frame `track_body` solved, read back from its JSON: the masked bones, each part's confidence and the fingers.
fn read_solved(value: &Value) -> BodyFrame {
    let mut frame = BodyFrame::empty(1, omuzaki_core::time::Seconds(0.0));
    for bone in HumanoidBone::ALL {
        if let Some(q) = value["bones"][format!("{bone:?}")].as_array() {
            let q: Vec<f32> = q.iter().map(|v| v.as_f64().unwrap() as f32).collect();
            frame.pose.set(bone, Quat::from_xyzw(q[0], q[1], q[2], q[3]));
            frame.mask = frame.mask.with(bone);
        }
    }
    let pair = |key: &str| -> [f32; 2] { serde_json::from_value(value[key].clone()).unwrap() };
    frame.arms = pair("arms");
    frame.hands = pair("hands");
    frame.body = value["body"].as_f64().unwrap() as f32;
    frame.fingers = serde_json::from_value::<[Option<Fingers>; 2]>(value["fingers"].clone()).unwrap();
    frame.head_relative = value["head_relative"].as_bool().unwrap();
    frame
}

/// A source that sends the same frame on every poll, as a tracker holding still on a still does.
struct Holding(BodyFrame);

impl BodySource for Holding {
    fn poll(&mut self) -> Option<BodyFrame> {
        Some(self.0)
    }
}

/// The character's pose once the stage's tracked-body layer has settled on `frame`: half a second at 60 Hz from the
/// rest pose, so every part has eased in to the tracker's confidence, then the neck's counter-turn.
fn as_the_stage_shows(frame: &BodyFrame) -> HumanoidPose {
    let mut body = TrackedBody::new(Box::new(Holding(*frame)));
    for i in 0..30 {
        body.step(Tick::at_rate(i, 60.0), false);
    }
    let mut pose = HumanoidPose::REST;
    body.apply(&mut pose);
    body.settle_head(&mut pose);
    pose
}

/// How far two pictures of one size are apart, pixel by pixel: a pixel's distance is its largest channel difference.
#[derive(Debug, Default, Clone, Copy)]
struct PixelDiff {
    pixels: u64,
    /// Pixels that differ at all.
    differ: u64,
    /// Pixels more than 16 levels off.
    far: u64,
    /// The sum of every channel's absolute difference.
    sum: u64,
    /// The largest distance.
    max: u8,
}

impl PixelDiff {
    fn add(&mut self, other: Self) {
        self.pixels += other.pixels;
        self.differ += other.differ;
        self.far += other.far;
        self.sum += other.sum;
        self.max = self.max.max(other.max);
    }

    fn line(&self) -> String {
        let share = |n: u64| 100.0 * n as f64 / self.pixels.max(1) as f64;
        format!(
            "{} px differ ({:.2}%), {} more than 16 levels ({:.2}%), mean {:.3} levels, max {}",
            self.differ,
            share(self.differ),
            self.far,
            share(self.far),
            self.sum as f64 / (3 * self.pixels.max(1)) as f64,
            self.max,
        )
    }
}

/// The distance between `ours` (RGBA) and `frame` (RGB), and the difference map as RGB: black where they match,
/// green for up to 16 levels off, red beyond, brighter the further off.
fn pixel_diff(ours: &omuzaki_render_api::Image, frame: &[u8]) -> (PixelDiff, Vec<u8>) {
    let ours = ours.rgba8.as_chunks::<4>().0;
    let theirs = frame.as_chunks::<3>().0;
    assert_eq!(ours.len(), theirs.len(), "the re-render and the frame are one size");
    let mut diff = PixelDiff { pixels: ours.len() as u64, ..PixelDiff::default() };
    let mut map = Vec::with_capacity(theirs.len() * 3);
    for (a, b) in ours.iter().zip(theirs) {
        let d: [u8; 3] = std::array::from_fn(|c| a[c].abs_diff(b[c]));
        let far = d.into_iter().max().unwrap_or(0);
        diff.sum += d.iter().map(|&v| u64::from(v)).sum::<u64>();
        diff.max = diff.max.max(far);
        diff.differ += u64::from(far > 0);
        diff.far += u64::from(far > 16);
        map.extend_from_slice(&match far {
            0 => [0, 0, 0],
            1..=16 => [0, 96 + 9 * far, 0],
            _ => [(64 + u16::from(far) * 3 / 4).min(255) as u8, 0, 0],
        });
    }
    (diff, map)
}

/// `ours` (RGBA) laid at half over `frame` (RGB), as RGB.
fn overlay(ours: &omuzaki_render_api::Image, frame: &[u8]) -> Vec<u8> {
    let ours = ours.rgba8.as_chunks::<4>().0;
    ours.iter()
        .zip(frame.as_chunks::<3>().0)
        .flat_map(|(a, b)| std::array::from_fn::<u8, 3, _>(|c| u8::midpoint(a[c], b[c])))
        .collect()
}

#[allow(clippy::disallowed_methods, reason = "test evidence, written to a folder the developer names; not an op")]
fn write_ppm(path: &std::path::Path, width: u32, height: u32, rgb: &[u8]) {
    let mut out = format!("P6\n{width} {height}\n255\n").into_bytes();
    out.extend_from_slice(rgb);
    std::fs::write(path, out).unwrap();
}

#[test]
#[allow(clippy::disallowed_methods, reason = "test evidence, read from and written to folders the developer names")]
fn the_tracked_poses_rerender_onto_their_frames_pixel_for_pixel_when_asked() {
    let env = |name: &str| std::env::var_os(name).map(std::path::PathBuf::from);
    let (Some(frames), Some(solved)) = (env("OMUZAKI_BODY_FRAMES_DIR"), env("OMUZAKI_BODY_SOLVED_DIR")) else {
        eprintln!("skipped: set OMUZAKI_BODY_FRAMES_DIR and OMUZAKI_BODY_SOLVED_DIR (track_body's solved frames)");
        return;
    };
    let out = env("OMUZAKI_BODY_PARITY_DIR");
    if let Some(out) = &out {
        std::fs::create_dir_all(out).unwrap();
    }
    let mut renderer = renderer();
    let (builder, def) = (CachingBuilder::new(), kit_def());
    let built = build(&builder, &def);
    let (light, shot) = (light(), shot(&built));
    let truths: std::collections::BTreeMap<String, HumanoidPose> = poses(&built).into_iter().collect();
    let (mut control_total, mut tracked_total, mut matched) = (PixelDiff::default(), PixelDiff::default(), 0);
    println!("frame | control (the truth re-rendered) | tracked");
    for (name, truth) in &truths {
        let (Ok(bytes), Ok(text)) =
            (std::fs::read(frames.join(format!("{name}.ppm"))), std::fs::read(solved.join(format!("{name}.json"))))
        else {
            continue;
        };
        let (width, height, frame) = read_ppm(&bytes);
        let control = render(renderer.as_mut(), build(&builder, &def), &light, &shot, truth);
        let (control, _) = pixel_diff(&control, frame);
        let solved: Value = serde_json::from_slice(&text).unwrap();
        let ours =
            render(renderer.as_mut(), build(&builder, &def), &light, &shot, &as_the_stage_shows(&read_solved(&solved)));
        let (tracked, map) = pixel_diff(&ours, frame);
        println!("{name} | {} | {}", control.line(), tracked.line());
        if let Some(out) = &out {
            write_ppm(&out.join(format!("{name}-map.ppm")), width, height, &map);
            write_ppm(&out.join(format!("{name}-overlay.ppm")), width, height, &overlay(&ours, frame));
            write_ppm(&out.join(format!("{name}-tracked.ppm")), width, height, &ppm_rgb(&ours));
        }
        control_total.add(control);
        tracked_total.add(tracked);
        matched += usize::from(tracked.differ == 0);
    }
    println!("control, every frame: {}", control_total.line());
    println!("tracked, every frame: {}; {matched} frames at 0", tracked_total.line());
    assert!(control_total.pixels > 0, "no frame in {} has a solved frame in {}", frames.display(), solved.display());
    // The floor: re-rendering a frame's own pose gives that frame back, so the tracked map is the tracker's alone.
    assert_eq!(control_total.differ, 0, "the renderer is not deterministic: {}", control_total.line());
}

/// RGBA as RGB.
fn ppm_rgb(image: &omuzaki_render_api::Image) -> Vec<u8> {
    image.rgba8.as_chunks::<4>().0.iter().flat_map(|p| [p[0], p[1], p[2]]).collect()
}

/// The coarse level's picture: a quarter of the frame's sides.
const COARSE: u32 = 4;

/// The kit character on one live stage per level, redrawn in each pose it is asked for exactly as a still is drawn.
struct StageSynth {
    renderer: Box<dyn Renderer>,
    stages: Vec<Stage>,
    rgb: Vec<u8>,
}

impl StageSynth {
    fn new(
        mut renderer: Box<dyn Renderer>,
        builder: &CachingBuilder,
        def: &CharacterDef,
        light: &StillLight,
        shot: &Shot,
    ) -> Self {
        let mut stages = Vec::new();
        // The coarse level only steers the big steps, so it draws at the live preset; the fine one as a still.
        for (size, quality) in
            [(UVec2::new(1280 / COARSE, 720 / COARSE), Quality::live()), (UVec2::new(1280, 720), Quality::offline())]
        {
            let r = renderer.as_mut();
            let target = r.create_target(&TargetDesc { size, alpha: AlphaEncoding::Straight }).unwrap();
            let mut scene = Scene::new(shot.camera);
            scene.lights.clone_from(&light.lights);
            scene.background = light.background;
            scene.post = light.post;
            scene.outline_light = light.outline_light;
            scene.back_light_rim = light.back_light_rim;
            scene.edges = light.edges;
            scene.occlusion = light.occlusion;
            let built = build(builder, def);
            let mut stage =
                Stage::new(r, built, UploadOptions::default(), scene, target, quality, StageClock::fixed(60.0))
                    .unwrap();
            stage.set_face(r, FaceRig::standard()).unwrap();
            stages.push(stage);
        }
        Self { renderer, stages, rgb: Vec::new() }
    }

    /// Takes every stage off the renderer.
    fn release(mut self) {
        for stage in self.stages.drain(..) {
            stage.release(self.renderer.as_mut()).unwrap();
        }
    }
}

impl Synth for StageSynth {
    fn render(&mut self, pose: &HumanoidPose, level: Level) -> Result<&[u8], String> {
        let stage = &mut self.stages[usize::from(level == Level::Fine)];
        let r = self.renderer.as_mut();
        // The still ticks on the default channels and redraws on the neutral face.
        let input = FrameInput { pose: *pose, ..FrameInput::default() };
        let shown = FrameInput { pose: *pose, channels: ChannelBuf::NEUTRAL, ..FrameInput::default() };
        // As a fresh stage on its 60 Hz clock: the springs and runtimes at rest under the pose, frame 0, the warm-up, frame
        // 1, and the redraw at frame 1, so nothing an earlier pose left behind reaches the pixels.
        let _ = stage.pin_time(Tick { index: 0, time: omuzaki_core::time::Seconds(0.0), dt: 0.0 });
        stage.advance(r, &input).map_err(|e| e.to_string())?;
        stage.settle_from_rest(0);
        let _ = stage.pin_time(Tick::at_rate(0, 60.0));
        stage.advance(r, &input).map_err(|e| e.to_string())?;
        stage.settle(SETTLE_STEPS);
        let _ = stage.pin_time(Tick::at_rate(1, 60.0));
        stage.advance(r, &input).map_err(|e| e.to_string())?;
        stage.redraw(r, &shown).map_err(|e| e.to_string())?;
        let image = r.read_target(stage.target()).map_err(|e| e.to_string())?;
        self.rgb.clear();
        self.rgb.extend(image.rgba8.as_chunks::<4>().0.iter().flat_map(|p| [p[0], p[1], p[2]]));
        Ok(&self.rgb)
    }
}

/// `rgb` (`width` wide) shrunk by [`COARSE`] on each side, each pixel the mean of its block.
fn shrink(rgb: &[u8], width: u32, height: u32) -> Vec<u8> {
    let (w, h, k) = (width as usize / COARSE as usize, height as usize / COARSE as usize, COARSE as usize);
    let mut out = Vec::with_capacity(w * h * 3);
    for y in 0..h {
        for x in 0..w {
            for c in 0..3 {
                let mut sum = 0_u32;
                for dy in 0..k {
                    for dx in 0..k {
                        sum += u32::from(rgb[((y * k + dy) * width as usize + x * k + dx) * 3 + c]);
                    }
                }
                out.push(((sum + (k * k / 2) as u32) / (k * k) as u32) as u8);
            }
        }
    }
    out
}

#[test]
#[allow(clippy::disallowed_methods, reason = "test evidence, read from and written to folders the developer names")]
fn the_body_fits_onto_its_frames_pixel_for_pixel_when_asked() {
    let env = |name: &str| std::env::var_os(name).map(std::path::PathBuf::from);
    let (Some(frames), Some(solved), Some(which)) =
        (env("OMUZAKI_BODY_FRAMES_DIR"), env("OMUZAKI_BODY_SOLVED_DIR"), std::env::var("OMUZAKI_BODY_FIT").ok())
    else {
        eprintln!("skipped: set OMUZAKI_BODY_FRAMES_DIR, OMUZAKI_BODY_SOLVED_DIR and OMUZAKI_BODY_FIT");
        return;
    };
    let out = env("OMUZAKI_BODY_PARITY_DIR");
    let (builder, def) = (CachingBuilder::new(), kit_def());
    let built = build(&builder, &def);
    let (light, shot) = (light(), shot(&built));
    let truths: std::collections::BTreeMap<String, HumanoidPose> = poses(&built).into_iter().collect();
    let mut stance = HumanoidPose::REST;
    omuzaki_stage::frame::pose::seed_base_pose(&built.rig, &mut stance);
    let mut synth = StageSynth::new(renderer(), &builder, &def, &light, &shot);
    let mut config = FitConfig::default();
    if let Some(renders) = std::env::var("OMUZAKI_BODY_FIT_RENDERS").ok().and_then(|v| v.parse().ok()) {
        config.renders = renders;
    }
    let mut checked = false;
    for (name, truth) in &truths {
        if !["all", "control"].contains(&which.as_str()) && !which.split(',').any(|w| w == name) {
            continue;
        }
        let (Ok(bytes), Ok(text)) =
            (std::fs::read(frames.join(format!("{name}.ppm"))), std::fs::read(solved.join(format!("{name}.json"))))
        else {
            continue;
        };
        let (width, height, frame) = read_ppm(&bytes);
        if !checked || which == "control" {
            // The synth draws a pose exactly as a still does: the frame's own pose gives the frame back.
            let ours = synth.render(truth, Level::Fine).unwrap();
            let control = omuzaki_track::fit::Score::of(ours, frame);
            assert_eq!(control.differ, 0, "{name}: the synth is not the still's drawing: {control:?}");
            println!("{name}: the frame's own pose redrawn: {control:?}");
            checked = true;
            if which == "control" {
                continue;
            }
        }
        let coarse = shrink(frame, width, height);
        // From what the tracker solved, or the character's own stance (as sculpted) when that scores better.
        let tracked = as_the_stage_shows(&read_solved(&serde_json::from_slice(&text).unwrap()));
        let starts = [tracked, stance];
        let began = std::time::Instant::now();
        let targets = Targets { coarse: &coarse, coarse_width: width / COARSE, fine: frame };
        let fitted = fit(&mut synth, targets, &starts, &config).unwrap();
        let share = |n: u64| 100.0 * n as f64 / f64::from(width * height);
        println!(
            "{name}: from {} px ({:.2}%) to {} px ({:.2}%) differing, sum {} to {}; {} coarse and {} fine renders in {:.0} s",
            fitted.start.differ,
            share(fitted.start.differ),
            fitted.score.differ,
            share(fitted.score.differ),
            fitted.start.sum,
            fitted.score.sum,
            fitted.renders[0],
            fitted.renders[1],
            began.elapsed().as_secs_f64(),
        );
        for (step, level, score, renders) in &fitted.trace {
            println!("  {step} deg {level:?}: sum {} differ {} after {renders} renders", score.sum, score.differ);
        }
        if let Some(out) = &out {
            std::fs::create_dir_all(out).unwrap();
            let ours = synth.render(&fitted.pose, Level::Fine).unwrap().to_vec();
            let image = omuzaki_render_api::Image {
                size: UVec2::new(width, height),
                alpha: AlphaEncoding::Straight,
                rgba8: ours.as_chunks::<3>().0.iter().flat_map(|p| [p[0], p[1], p[2], 255]).collect(),
            };
            let (_, map) = pixel_diff(&image, frame);
            write_ppm(&out.join(format!("{name}-fit-map.ppm")), width, height, &map);
            write_ppm(&out.join(format!("{name}-fit.ppm")), width, height, &ours);
        }
    }
    synth.release();
}
