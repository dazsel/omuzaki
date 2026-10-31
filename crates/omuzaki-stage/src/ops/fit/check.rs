//! `fit.check` (#331, #2604): poses a kit character through a battery of stress poses (the spine bent forward, back and
//! sideways, twisted, a fall, the arms up and crossed) and says, for each, how much of the body its outfit covers in the
//! bind pose shows through once posed, and where. An agent runs it after it changes a body or an outfit; a person
//! reads it as a list of poses with a share and a place (`the stomach, 12 cm², bending back`).

use omuzaki_anim_api::{IkGoals, Tick};
use omuzaki_char_api::CharId;
use omuzaki_ops_api::{CostHint, Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::body::Body;
use crate::fit::{FitCheck, battery};
use crate::frame::rig::RigFrame;
use crate::ops::motion::arm_room::load;

/// How much shown body a pose may have and still fit, square centimetres: a few stray points at a crease.
pub const FITS_CM2: f32 = 1.0;
/// The most places listed for a pose.
const MAX_REGIONS: usize = 4;

/// Pose a character through the stress battery and report where its body shows through its outfit.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        {"character": "kit"},
        {"character": "kit", "poses": ["bend-back-30", "fall-back"]}
    ],
    "x-omuzaki-fixture" = "character"
))]
pub struct CheckFit {
    /// The character: a kit character (an imported model's outfit is part of its own mesh and isn't checked).
    pub character: CharId,
    /// The poses to check, by name: `rest`, `bend-forward-30`, `bend-back-30`, `bend-left-30`, `bend-right-30`,
    /// `twist-left-30`, `twist-right-30`, `fall-back`, `arms-up`, `arms-crossed`. Default: all of them.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub poses: Option<Vec<String>>,
}

/// Where something shows in one pose.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ShownAt {
    /// What shows: `body`, or a garment part showing through the one over it.
    pub layer: String,
    /// The bone that moves it there, such as `spine`.
    pub bone: String,
    /// How much shows there, square centimetres.
    pub cm2: f32,
}

/// One pose's result.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PoseResult {
    /// The pose.
    pub pose: String,
    /// Whether the outfit covers the body and its own inner layers in it: at most 1 cm² of each shows.
    pub fits: bool,
    /// The covered body that shows, square centimetres.
    pub body_cm2: f32,
    /// The share of the covered body that shows, 0 to 1.
    pub body_share: f32,
    /// Inner garments that show through the ones over them (tights through a skirt), square centimetres.
    pub layers_cm2: f32,
    /// The deepest anything stands out through the cloth over it, millimetres (0 where it only shows through a gap).
    pub deepest_mm: f32,
    /// Where it shows, largest first.
    pub at: Vec<ShownAt>,
    /// The middle of the largest place that shows, on the character in its bind pose (metres, the character's own
    /// space): where to point a camera.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub look_at: Option<[f32; 3]>,
}

/// One layer of the outfit.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct LayerCover {
    /// `body`, or a garment part.
    pub layer: String,
    /// How much of it the layers over it cover in the bind pose, square centimetres.
    pub covered_cm2: f32,
}

/// What `fit.check` returns.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CheckFitResult {
    /// The character.
    pub character: CharId,
    /// Whether every pose fits.
    pub fits: bool,
    /// The body the outfit covers in the bind pose, square centimetres (0 for a character with no garment over its
    /// body: then nothing can show and every pose fits).
    pub covered_cm2: f32,
    /// Each layer, the body and then each garment inner first, with how much of it the layers over it cover in the
    /// bind pose.
    pub layers: Vec<LayerCover>,
    /// Each pose, in the battery's order.
    pub poses: Vec<PoseResult>,
    /// The pose where most shows, if any shows.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worst: Option<String>,
}

impl CheckFit {
    fn measure(&self, body: &Body) -> Result<CheckFitResult, OpError> {
        let Body::Kit(built) = body else {
            return Err(OpError::invalid(
                "character",
                format!("'{}' is an imported model: its outfit is part of its own mesh", self.character),
            )
            .hint("fit.check checks kit characters, whose garments are parts"));
        };
        let mut frame = RigFrame::for_character(built)
            .map_err(|e| OpError::invalid("character", format!("its rig doesn't build: {e}")))?;
        let check = FitCheck::new(built);
        let all = battery(&body.base_pose());
        let chosen: Vec<_> = match &self.poses {
            None => all,
            Some(names) => {
                let mut out = Vec::with_capacity(names.len());
                for name in names {
                    let pose = all.iter().find(|p| p.name == name).ok_or_else(|| {
                        let known: Vec<_> = all.iter().map(|p| p.name).collect();
                        OpError::invalid("poses", format!("there is no stress pose '{name}'"))
                            .hint(format!("one of {}", known.join(", ")))
                    })?;
                    out.push(pose.clone());
                }
                out
            }
        };
        let mut poses = Vec::with_capacity(chosen.len());
        for stress in &chosen {
            frame.reset();
            frame.solve(Tick::at_rate(0, 60.0), &stress.pose, &IkGoals::NONE);
            let fit = check.check(frame.skin());
            poses.push(PoseResult {
                pose: stress.name.to_owned(),
                fits: fit.body_cm2 <= FITS_CM2 && fit.layers_cm2 <= FITS_CM2,
                body_cm2: fit.body_cm2,
                body_share: fit.body_share,
                layers_cm2: fit.layers_cm2,
                deepest_mm: fit.deepest_mm,
                at: fit
                    .shown
                    .into_iter()
                    .take(MAX_REGIONS)
                    .map(|s| ShownAt { layer: s.layer, bone: s.bone, cm2: s.cm2 })
                    .collect(),
                look_at: fit.worst_at.map(|p| p.to_array()),
            });
        }
        let shown = |p: &PoseResult| p.body_cm2 + p.layers_cm2;
        let worst = poses.iter().filter(|p| shown(p) > 0.0).max_by(|a, b| shown(a).total_cmp(&shown(b)));
        Ok(CheckFitResult {
            character: self.character.clone(),
            fits: poses.iter().all(|p| p.fits),
            covered_cm2: check.covered_cm2(),
            layers: check.layers().map(|(layer, cm2)| LayerCover { layer: layer.to_owned(), covered_cm2: cm2 }).collect(),
            worst: worst.map(|p| p.pose.clone()),
            poses,
        })
    }
}

impl Op for CheckFit {
    fn meta() -> OpMeta {
        OpMeta {
            name: "fit.check",
            summary: "Pose a kit character through a battery of stress poses (bends, twists, a fall, arms up and \
                      crossed) and report, per pose, how much of the body its outfit covers shows through once posed, \
                      and where.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        if ctx.doc.get(&omuzaki_char_api::paths::character(&self.character)).is_none() {
            return Err(OpError::not_found(format!("there is no character '{}'", self.character))
                .hint("check the id; characters live under characters/<id>"));
        }
        let body = load(ctx, &self.character)?;
        let result = self.measure(&body)?;
        let output = serde_json::to_value(result).map_err(|e| OpError::internal(format!("the fit check: {e}")))?;
        Ok(Plan {
            output: Some(output),
            cost: Some(CostHint { seconds: Some(2.0), gpu_mib: Some(0), usd: None }),
            ..Plan::default()
        })
    }
}

register_op!(CheckFit);
