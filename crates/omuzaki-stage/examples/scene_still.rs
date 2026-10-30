//! A scene built from ops, rendered to a PNG: `cargo run -p omuzaki-stage --features wgpu --example scene_still --
//! RECIPE.json OUT.png`.
//!
//! The reference loop's render step for sets, lights and props. A recipe is the list of registered ops a person or an
//! agent would run (`character.create`, `stage.create_scene`, `stage.set_stage_preset`, `stage.add_prop`,
//! `stage.set_camera`, ...), plus the picture to take:
//!
//! ```json
//! {
//!   "ops": [{ "op": "character.create", "args": { "id": "kit", "template": "standard", "seed": 7 } }, ...],
//!   "scene": "main",
//!   "size": [540, 960],
//!   "quality": "offline",
//!   "frames": 3
//! }
//! ```
//!
//! The ops run in order on an empty project, each planned and applied as the executor would; the scene `scene` is
//! staged on the software (or `WGPU_ADAPTER_NAME`) adapter at `size`, `frames` frames run from rest, and the last one is
//! written as a PNG. `quality` is `live`, `high` or `offline` (16 samples). The recipe and the PNG stay wherever they are
//! given: a recipe that recreates a reference's set is reference material, so keep it, the picture and any overlay out
//! of the repository.

use std::error::Error;
use std::path::PathBuf;

use omuzaki_char::builder::CachingBuilder;
use omuzaki_char_api::{BuildServices, Quality as BuildQuality};
use omuzaki_core::glam::UVec2;
use omuzaki_core::time::FakeClock;
use omuzaki_doc_api::DocWrite;
use omuzaki_doc_mock::MemDoc;
use omuzaki_ops_api::{Actor, OpCtx, find};
use omuzaki_render::{AdapterPreference, HeadlessOptions, WgpuRenderer};
use omuzaki_render_api::{AlphaEncoding, Quality, Renderer, TargetDesc};
use omuzaki_stage::body::BodyServices;
use omuzaki_stage::frame::{FrameInput, StageClock};
use omuzaki_stage::scene::stage::{Bodies, SceneStage, SceneStageOptions};
use omuzaki_stage::stages::FrameLoop;
use omuzaki_stage_api::SceneId;
use omuzaki_store_mock::MemStore;
use serde::Deserialize;
use serde_json::{Value, json};

/// One op of the recipe: its registered name and its arguments.
#[derive(Deserialize)]
struct Step {
    op: String,
    #[serde(default)]
    args: Value,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Recipe {
    ops: Vec<Step>,
    #[serde(default = "main_scene")]
    scene: String,
    #[serde(default = "portrait")]
    size: [u32; 2],
    #[serde(default = "offline")]
    quality: String,
    #[serde(default = "three")]
    frames: usize,
}

fn main_scene() -> String {
    "main".to_owned()
}

fn portrait() -> [u32; 2] {
    [540, 960]
}

fn offline() -> String {
    "offline".to_owned()
}

fn three() -> usize {
    3
}

#[allow(clippy::disallowed_methods)] // a command-line tool reads the recipe and writes the picture the user names
fn main() -> Result<(), Box<dyn Error>> {
    let mut args = std::env::args_os().skip(1);
    let (Some(recipe), Some(out)) = (args.next(), args.next()) else {
        return Err("usage: scene_still RECIPE.json OUT.png".into());
    };
    let recipe: Recipe = serde_json::from_slice(&std::fs::read(&recipe)?)?;
    let quality = match recipe.quality.as_str() {
        "live" => Quality::live(),
        "high" => Quality::high(),
        "offline" => Quality::offline(),
        other => return Err(format!("quality `{other}`: live, high or offline").into()),
    };

    omuzaki_stage::link();
    let clock = FakeClock::default();
    let actor = Actor::Person { name: "recipe".to_owned() };
    let mut doc = MemDoc::from_value(json!({}));
    for (i, step) in recipe.ops.into_iter().enumerate() {
        let entry = find(&step.op).ok_or_else(|| format!("op {i}: `{}` is not registered", step.op))?;
        let op = (entry.decode)(step.args).map_err(|e| format!("op {i} ({}): {e:?}", step.op))?;
        let plan = op.plan(&OpCtx::new(&doc, &actor, &clock)).map_err(|e| format!("op {i} ({}): {e:?}", step.op))?;
        doc.apply(&plan.patch)?;
    }

    let mut renderer = WgpuRenderer::headless(&HeadlessOptions { adapter: AdapterPreference::Software })?;
    let size = UVec2::from_array(recipe.size);
    let store = MemStore::new();
    let mtoon = renderer.capabilities().mtoon;
    let (builder, build) = (CachingBuilder::new(), BuildServices::default());
    let mut bodies = Bodies::new(BodyServices::new(&builder, &build, &store, mtoon, &clock), BuildQuality::Live);
    let target = renderer.create_target(&TargetDesc { size, alpha: AlphaEncoding::Straight })?;
    #[allow(clippy::cast_precision_loss)] // picture sizes are far below 2^24
    let aspect = size.x as f32 / size.y as f32;
    let options = SceneStageOptions::new(quality, StageClock::fixed(60.0));
    let scene = SceneId::new(&recipe.scene)?;
    let mut stage = SceneStage::open(&doc, Some(&scene), &mut bodies, &mut renderer, target, aspect, options)?;
    for name in stage.skipped() {
        eprintln!("left off the stage: {name:?}");
    }
    let input = FrameInput { dt: 1.0 / 60.0, ..FrameInput::default() };
    for _ in 0..recipe.frames.max(1) {
        stage.tick(&mut renderer, &input)?;
    }
    let image = renderer.read_target(target)?;
    std::fs::write(PathBuf::from(out), omuzaki_stage::ops::render::still::encode_png(&image)?)?;
    stage.release(&mut renderer)?;
    renderer.release(target.into())?;
    Ok(())
}
