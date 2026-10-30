//! The 3D eye's quick edits. `face.eye3d.paint` paints marks by hand on its skin, white and iris, placed exactly.
//! `face.eye3d.preset` gives a character's eye a named shape and iris colour in one undoable
//! edit, the first step before its numbers are tuned with `face.set_style`.
//!
//! The fit (`face.eye3d.fit`): an agent measures landmarks on a photograph of an eye (the opening's outline,
//! the iris, the pupil, in the picture's pixels) and the op fits the character's 3D eye to them: its opening's shape,
//! its iris's and pupil's sizes, and the shot (camera and gaze) that frames it the same way, which `render.eye` takes.
//! Only the numbers are used, never the picture: the eye is still drawn by its design. One undoable edit of
//! `face/eye3d`.

use omuzaki_char_api::{CharId, paths};
use omuzaki_doc_api::Patch;
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use omuzaki_face_api::eye3d::{EyeMark, EyeShape, IrisColour};

use crate::eye3d::closeup::{Shot, View};
use crate::eye3d::fit::{Landmarks, fit};
use crate::ops_shared::check::check_edit;
use crate::ops_shared::edit::{changed_output, diff, effective, stored_face, to_json, working_copy};
use crate::ops_shared::path::template;

/// Gives a character's 3D eye a named shape, a named iris colour, or both.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        {"character": "kit", "shape": "wide", "iris": "amber"},
        {"character": "kit", "iris": "green"},
        {"character": "kit", "shape": "hooded"}
    ],
    "x-omuzaki-fixture" = "character"
))]
pub struct EyePreset {
    /// The character whose 3D eye to set (it gets one with the defaults if it has none).
    pub character: CharId,
    /// The opening's shape: `natural`, `wide`, `almond`, `round`, `hooded` or `downturned`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shape: Option<EyeShape>,
    /// The iris's colour: `amber`, `brown`, `hazel`, `green`, `blue` or `grey`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub iris: Option<IrisColour>,
}

impl Op for EyePreset {
    fn meta() -> OpMeta {
        OpMeta {
            name: "face.eye3d.preset",
            summary: "Give a character's 3D eye a named shape (natural, wide, almond, round, hooded, downturned) and/or \
                      a named iris colour (amber, brown, hazel, green, blue, grey) in one undoable edit; every other \
                      field keeps its value, and face.set_style tunes it from there",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        if self.shape.is_none() && self.iris.is_none() {
            return Err(OpError::invalid("shape", "give a shape, an iris colour or both"));
        }
        let stored = stored_face(ctx, &self.character)?;
        let mut design = effective(stored.as_ref())?.eye3d.unwrap_or_default();
        if let Some(shape) = self.shape {
            shape.apply(&mut design);
        }
        if let Some(iris) = self.iris {
            iris.apply(&mut design);
        }
        let mut working = working_copy(stored.as_ref());
        if let Value::Object(face) = &mut working {
            face.insert("eye3d".to_owned(), to_json(&design)?);
        }
        check_edit(stored.as_ref(), &working, &template()?)?;
        let mut patch = Patch::new();
        diff(&paths::face(&self.character), stored.as_ref(), Some(&working), &mut patch);
        let mut plan = Plan::edit(patch);
        let mut output = changed_output(&plan.patch, None);
        if let Value::Object(fields) = &mut output {
            fields.insert("next".to_owned(), json!("render.eye to see it"));
        }
        plan.output = Some(output);
        Ok(plan)
    }
}

register_op!(EyePreset);

/// Paints marks by hand on a character's 3D eye: on its skin, its white or its iris.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        {"character": "kit", "marks": [
            {"on": "skin", "points": [[0.016, -0.009]], "size": 0.0006, "colour": "#6a3a24", "opacity": 0.7, "softness": 0.6},
            {"on": "sclera", "points": [[0.011, 0.0], [0.009, 0.0004], [0.0075, 0.0003]], "size": 0.00008,
             "colour": "#a03a3a", "opacity": 0.5, "taper": 0.8},
            {"on": "iris", "points": [[0.0025, 0.0012]], "size": 0.0004, "colour": "#3a2412", "opacity": 0.8}
        ]},
        {"character": "kit", "marks": [], "replace": true}
    ],
    "x-omuzaki-fixture" = "character"
))]
pub struct PaintEye {
    /// The character whose 3D eye to paint (it gets one with the defaults if it has none).
    pub character: CharId,
    /// The marks, painted in order over the ones already there.
    pub marks: Vec<EyeMark>,
    /// Whether the marks replace every mark already painted (an empty list clears them) rather than adding to them.
    #[serde(default)]
    pub replace: bool,
}

impl Op for PaintEye {
    fn meta() -> OpMeta {
        OpMeta {
            name: "face.eye3d.paint",
            summary: "Paint marks by hand on a character's 3D eye: dots and strokes on its skin, its white or its iris \
                      (freckles, moles, vessels, crypts, flecks), each placed in metres in the eye's frame, over the \
                      detail the design generates; adds to the marks already there, or replaces them. One undoable \
                      edit",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        if self.marks.is_empty() && !self.replace {
            return Err(OpError::invalid("marks", "give at least one mark, or `replace: true` to clear them"));
        }
        let stored = stored_face(ctx, &self.character)?;
        let mut design = effective(stored.as_ref())?.eye3d.unwrap_or_default();
        if self.replace {
            design.paint.clear();
        }
        design.paint.extend(self.marks.iter().cloned());
        let count = design.paint.len();
        let mut working = working_copy(stored.as_ref());
        if let Value::Object(face) = &mut working {
            face.insert("eye3d".to_owned(), to_json(&design)?);
        }
        check_edit(stored.as_ref(), &working, &template()?)?;
        let mut patch = Patch::new();
        diff(&paths::face(&self.character), stored.as_ref(), Some(&working), &mut patch);
        let mut plan = Plan::edit(patch);
        let mut output = changed_output(&plan.patch, None);
        if let Value::Object(fields) = &mut output {
            fields.insert("marks".to_owned(), json!(count));
            fields.insert("next".to_owned(), json!("render.eye to see them"));
        }
        plan.output = Some(output);
        Ok(plan)
    }
}

register_op!(PaintEye);

fn rounds() -> u32 {
    200
}

/// Fits a character's 3D eye to landmarks measured on a photograph.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [{
        "character": "kit",
        "landmarks": {
            "image": [1024, 744],
            "outline": [[140, 330], [250, 270], [350, 220], [450, 178], [550, 167], [630, 225], [700, 330],
                        [722, 470], [680, 500], [580, 480], [480, 450], [380, 413], [280, 378], [180, 344]],
            "iris": { "centre": [522, 306], "radius": 138 },
            "pupil": { "centre": [516, 304], "radius": 42 },
            "ball_edge": [[708, 320], [720, 360], [725, 400], [724, 440], [717, 480]]
        }
    }],
    "x-omuzaki-fixture" = "character"
))]
pub struct FitEye {
    /// The character whose 3D eye is fitted (it gets one with the defaults if it has none).
    pub character: CharId,
    /// What was measured on the picture.
    pub landmarks: Landmarks,
    /// The shot the fit starts from; the `macro` view's when left out.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start: Option<Shot>,
    /// The most passes over the parameters, 1 to 1000 (200 when left out; the fit stops sooner once it settles).
    #[serde(default = "rounds")]
    pub rounds: u32,
}

impl Op for FitEye {
    fn meta() -> OpMeta {
        OpMeta {
            name: "face.eye3d.fit",
            summary: "Fit a character's 3D eye to landmarks measured on a photograph (the opening's outline, the iris \
                      and the pupil, and where it shows, the eyeball's own edge, in its pixels): set its opening's \
                      shape and its eyeball's, iris's and pupil's sizes, and return the shot (camera and gaze) that \
                      frames it the same way, for render.eye, with the remaining error in pixels",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        self.landmarks.check().map_err(|e| OpError::invalid("landmarks", e))?;
        if !(1..=1000).contains(&self.rounds) {
            return Err(OpError::invalid("rounds", "must be 1 to 1000"));
        }
        let start = self.start.clone().unwrap_or_else(|| View::Macro.shot());
        start.check().map_err(|e| OpError::invalid("start", e))?;
        let stored = stored_face(ctx, &self.character)?;
        let design = effective(stored.as_ref())?.eye3d.unwrap_or_default();
        let fitted = fit(&design, &start, &self.landmarks, self.rounds);

        let mut working = working_copy(stored.as_ref());
        if let Value::Object(face) = &mut working {
            face.insert("eye3d".to_owned(), to_json(&fitted.design)?);
        }
        check_edit(stored.as_ref(), &working, &template()?)?;
        let mut patch = Patch::new();
        diff(&paths::face(&self.character), stored.as_ref(), Some(&working), &mut patch);
        let mut plan = Plan::edit(patch);
        let mut output = changed_output(&plan.patch, None);
        if let Value::Object(fields) = &mut output {
            fields.insert("shot".to_owned(), to_json(&fitted.shot)?);
            fields.insert("residuals".to_owned(), to_json(&fitted.residuals)?);
            fields.insert("before".to_owned(), to_json(&fitted.before)?);
            fields.insert("next".to_owned(), json!("render.eye with this shot to see it framed as the picture"));
        }
        plan.output = Some(output);
        Ok(plan)
    }
}

register_op!(FitEye);
