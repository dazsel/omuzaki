//! `render.contact_sheet`: pictures laid out in a labelled grid as one PNG (spec v3 §11), so a person or an agent sees a
//! set of previews (variants, expressions, before and after) at a glance. The cells are stored renders, or one
//! character rendered here in each of several expressions or variants with one camera and one light rig.

use image::RgbaImage;
use omuzaki_anim_api::HumanoidPose;
use omuzaki_char_api::{BuildServices, CharId, CharacterBuilder, VariantId};
use omuzaki_core::glam::UVec2;
use omuzaki_core::time::SystemClock;
use omuzaki_face_api::ExpressionId;
use omuzaki_ops_api::{CostHint, JobCtx, Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use omuzaki_render_api::{Capabilities, Image, MtoonSupport};
use omuzaki_stage_api::{CameraDef, Framing};
use omuzaki_store_api::Store;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::job::{
    DEFAULT_LIGHT_RIG, RenderQuality, character_bytes, check_expression, check_size, cost, still_light, with_renderer,
};
use super::picture::{MAX_CANVAS_PIXELS, MAX_CANVAS_SIDE, canvas, hash_of, load, paste, store, straight};
use super::pose::PoseArg;
use super::still::{Shot, StillSpec, render_body_stills, straight_rgba};
use crate::body::{Body, BodyServices, load_body};
use crate::scene::frame::frame;
use crate::sheet::layout::{Cell, Page, Rect, SheetLayout, Theme};

/// The most pictures one sheet takes.
pub const MAX_IMAGES: usize = 64;
/// The most columns a sheet has.
pub const MAX_COLUMNS: u32 = 16;
/// The widest gap between cells, in pixels.
pub const MAX_GUTTER: u32 = 256;
/// The largest side of a rendered cell, in pixels: 16 columns of them still fit a sheet.
pub const MAX_CELL_SIDE: u32 = 1024;
/// The length of a stored picture's label: the start of its hash.
const HASH_LABEL: usize = 12;
/// The longest title a stored picture may be given.
const MAX_TITLE: usize = 48;
/// The resolution a labelled sheet's type is set at: a 9 pt label is 18 pixels to the em.
const LABEL_DPI: f32 = 144.0;
/// A rendered cell when none is given: a portrait cell.
const DEFAULT_CELL: [u32; 2] = [384, 512];

fn default_gutter() -> u32 {
    8
}

fn default_cell() -> [u32; 2] {
    DEFAULT_CELL
}

fn default_light_rig() -> String {
    DEFAULT_LIGHT_RIG.to_owned()
}

fn yes() -> bool {
    true
}

/// Lays pictures out in a labelled grid and stores the sheet as a PNG: stored renders, or a character rendered in each
/// of several expressions or variants.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        {"images": ["omuzaki://render/e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855.png", "omuzaki://render/e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855.png"]},
        {"character": "kit", "expressions": ["happy", "sad", "angry"], "framing": "face", "cell": [256, 256]},
        {"character": "kit", "variants": ["close-up"], "expression": "happy", "labels": false},
        {"character": "kit", "poses": [{"id": "wave-right"}, {"id": "wave-right", "mirror": true}, {"id": "wave-right", "blend": {"with": "wave-left", "weight": 0.5}}]}
    ],
    "x-omuzaki-fixture" = "character-variant"
))]
pub struct ContactSheet {
    /// Stored renders, by their `omuzaki://render/<hash>.png` URIs or hashes, in reading order: 1 to 64. Leave out to
    /// render `character` instead.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<String>,
    /// With `images`: a title for each, in the same order, written under it instead of the start of its hash (at most
    /// 48 characters each). Leave out to label each picture by its hash.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub titles: Vec<String>,
    /// A character, built or imported, to render one cell for each of `expressions`, `variants` or `poses`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub character: Option<CharId>,
    /// Expressions of the character's face profile, one cell each at full weight.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub expressions: Vec<ExpressionId>,
    /// Variants of a kit character, one cell each.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub variants: Vec<VariantId>,
    /// Poses of the pose library (`pose.list`), one cell each: a pose library sheet.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub poses: Vec<PoseArg>,
    /// With `expressions` or `poses`: the variant every cell shows, instead of the base.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variant: Option<VariantId>,
    /// With `variants` or `poses`: the expression every cell shows, instead of the neutral face.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expression: Option<ExpressionId>,
    /// With `expressions` or `variants`: the library pose every cell shows, instead of the character's own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pose: Option<PoseArg>,
    /// The shot of every rendered cell, from the front: `full` (the default), `bust` or `face`.
    #[serde(default)]
    pub framing: Framing,
    /// The light rig of every rendered cell, by name (`viewer-studio` by default).
    #[serde(default = "default_light_rig")]
    pub light_rig: String,
    /// A rendered cell's width and height in pixels, each 16 to 1024 (384 x 512 by default). A stored picture keeps
    /// its own size; the cells are as large as the largest.
    #[serde(default = "default_cell")]
    pub cell: [u32; 2],
    /// `live`, `high` or `offline` (the default, the same bytes on every run), for rendered cells.
    #[serde(default)]
    pub quality: RenderQuality,
    /// Cells per row, 1 to 16; by default the nearest to a square grid.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub columns: Option<u32>,
    /// The gap between cells and around the sheet, in pixels, 0 to 256 (8 by default).
    #[serde(default = "default_gutter")]
    pub gutter: u32,
    /// Write each cell's label under it, on a paper sheet: the expression, variant or pose, or the first 12 characters
    /// of a stored picture's hash. True by default; false lays the pictures alone on a transparent sheet.
    #[serde(default = "yes")]
    pub labels: bool,
}

/// The stored sheet and its grid.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ContactSheetResult {
    /// `omuzaki://render/<hash>.png` of the sheet.
    pub uri: String,
    /// Its content hash.
    pub hash: String,
    /// Its width and height in pixels.
    pub size: [u32; 2],
    /// Pictures per row.
    pub columns: u32,
    /// Rows.
    pub rows: u32,
    /// Each cell's picture width and height: the largest picture's. Smaller pictures are centred in theirs, at their
    /// own size.
    pub cell: [u32; 2],
    /// Each cell's label, in reading order, whether or not it is written on the sheet.
    pub cells: Vec<String>,
}

/// What the cells are.
enum Cells<'a> {
    /// Stored pictures.
    Images(&'a [String]),
    /// One character, one cell for each of something.
    Rendered { character: &'a CharId, varied: Varied<'a> },
}

/// What a rendered sheet varies from cell to cell.
enum Varied<'a> {
    /// The expression.
    Expressions(&'a [ExpressionId]),
    /// The variant.
    Variants(&'a [VariantId]),
    /// The pose.
    Poses(&'a [PoseArg]),
}

impl Varied<'_> {
    fn len(&self) -> usize {
        match self {
            Self::Expressions(list) => list.len(),
            Self::Variants(list) => list.len(),
            Self::Poses(list) => list.len(),
        }
    }

    fn field(&self) -> &'static str {
        match self {
            Self::Expressions(_) => "expressions",
            Self::Variants(_) => "variants",
            Self::Poses(_) => "poses",
        }
    }
}

/// The checked arguments.
struct Checked<'a> {
    cells: Cells<'a>,
    columns: u32,
    rows: u32,
}

/// `error` naming `field`.
fn at(field: String) -> impl FnOnce(OpError) -> OpError {
    move |mut error| {
        error.field = Some(field);
        error
    }
}

impl ContactSheet {
    fn check(&self) -> Result<Checked<'_>, OpError> {
        let cells = self.cells()?;
        let (n, field) = match &cells {
            Cells::Images(images) => (images.len(), "images"),
            Cells::Rendered { varied, .. } => (varied.len(), varied.field()),
        };
        if n == 0 || n > MAX_IMAGES {
            return Err(OpError::invalid(field, format!("{n} cells; a sheet takes 1 to {MAX_IMAGES}")));
        }
        match &cells {
            Cells::Images(images) => {
                for (i, image) in images.iter().enumerate() {
                    hash_of(&format!("images/{i}"), image)?;
                }
                if !self.titles.is_empty() && self.titles.len() != images.len() {
                    let reason =
                        format!("{} titles for {} images: give one each, or none", self.titles.len(), images.len());
                    return Err(OpError::invalid("titles", reason));
                }
                if let Some((i, long)) = self.titles.iter().enumerate().find(|(_, t)| t.chars().count() > MAX_TITLE) {
                    return Err(OpError::invalid(
                        format!("titles/{i}"),
                        format!("\"{long}\" is over {MAX_TITLE} characters"),
                    ));
                }
            }
            Cells::Rendered { varied, .. } => {
                self.check_render()?;
                if let Some(pose) = &self.pose {
                    pose.check("pose")?;
                }
                if let Varied::Poses(poses) = varied {
                    for (i, pose) in poses.iter().enumerate() {
                        pose.check(&format!("poses/{i}"))?;
                    }
                }
            }
        }
        if self.gutter > MAX_GUTTER {
            return Err(OpError::invalid("gutter", format!("a gutter of {} pixels is over {MAX_GUTTER}", self.gutter)));
        }
        let n = u32::try_from(n).unwrap_or(u32::MAX);
        let columns = match self.columns {
            Some(c) if c == 0 || c > MAX_COLUMNS => {
                return Err(OpError::invalid("columns", format!("{c} columns; a sheet has 1 to {MAX_COLUMNS}")));
            }
            Some(c) => c.min(n),
            None => (1..=n).find(|c| c * c >= n).unwrap_or(n),
        };
        Ok(Checked { cells, columns, rows: n.div_ceil(columns) })
    }

    /// Which cells the arguments ask for: stored images, or a character varied in one way, with the settings that way
    /// shares across its cells.
    fn cells(&self) -> Result<Cells<'_>, OpError> {
        let lists = [
            (!self.expressions.is_empty(), "expressions"),
            (!self.variants.is_empty(), "variants"),
            (!self.poses.is_empty(), "poses"),
        ];
        let mut given = lists.iter().filter(|(given, _)| *given).map(|(_, field)| *field);
        let Some(character) = &self.character else {
            let shared = [
                (self.variant.is_some(), "variant"),
                (self.expression.is_some(), "expression"),
                (self.pose.is_some(), "pose"),
            ];
            if given.next().is_some() || shared.iter().any(|(given, _)| *given) {
                return Err(OpError::invalid(
                    "character",
                    "expressions, variants and poses are rendered on a character: name it",
                ));
            }
            return Ok(Cells::Images(&self.images));
        };
        if !self.images.is_empty() {
            return Err(OpError::invalid("images", "give stored images or a character to render, not both"));
        }
        if !self.titles.is_empty() {
            return Err(OpError::invalid(
                "titles",
                "titles name stored images; a rendered cell is labelled by what it shows",
            ));
        }
        let Some(first) = given.next() else {
            return Err(OpError::invalid(
                "expressions",
                "give the expressions, variants or poses to render the character in",
            )
            .hint("for example \"expressions\": [\"happy\", \"sad\"]"));
        };
        if let Some(second) = given.next() {
            return Err(OpError::invalid(
                second,
                format!("a sheet varies one thing: give {first} or {second}, not both"),
            ));
        }
        let own = |field: &str, given: bool| {
            if given {
                Err(OpError::invalid(
                    field,
                    format!("each cell shows its own {field}; `{field}` goes with the other lists"),
                ))
            } else {
                Ok(())
            }
        };
        let varied = match first {
            "expressions" => {
                own("expression", self.expression.is_some())?;
                Varied::Expressions(&self.expressions)
            }
            "variants" => {
                own("variant", self.variant.is_some())?;
                Varied::Variants(&self.variants)
            }
            _ => {
                own("pose", self.pose.is_some())?;
                Varied::Poses(&self.poses)
            }
        };
        Ok(Cells::Rendered { character, varied })
    }

    /// The rendered cells' size, shot and light.
    fn check_render(&self) -> Result<UVec2, OpError> {
        let [x, y] = self.cell;
        if x > MAX_CELL_SIDE || y > MAX_CELL_SIDE {
            return Err(OpError::invalid("cell", format!("a {x}x{y} cell is larger than {MAX_CELL_SIDE} a side")));
        }
        let size = check_size(self.cell, self.quality).map_err(|mut e| {
            e.field = Some("cell".to_owned());
            e
        })?;
        if matches!(self.framing, Framing::Free | Framing::Hands) {
            return Err(OpError::invalid("framing", "a contact sheet frames the character: full, bust or face"));
        }
        still_light(&self.light_rig)?;
        Ok(size)
    }

    /// The sheet's size for `cell`-sized pictures, refused when it is too large to make.
    fn sheet_size(&self, checked: &Checked<'_>, cell: UVec2) -> Result<(UVec2, u32), OpError> {
        let band = if self.labels { label_band(checked, cell, self.gutter)? } else { 0 };
        let size = grid_size(checked, cell, self.gutter, band);
        let pixels = u64::from(size.x) * u64::from(size.y);
        if size.x > MAX_CANVAS_SIDE || size.y > MAX_CANVAS_SIDE || pixels > MAX_CANVAS_PIXELS {
            return Err(OpError::invalid(
                "cell",
                format!(
                    "the sheet would be {}x{} pixels; at most {MAX_CANVAS_SIDE} a side and {MAX_CANVAS_PIXELS} pixels",
                    size.x, size.y
                ),
            )
            .hint("use fewer columns or smaller cells"));
        }
        Ok((size, band))
    }
}

/// The sheet's size: the cells in their grid, each with a label band under it, and a gutter around every one.
fn grid_size(checked: &Checked<'_>, cell: UVec2, gutter: u32, band: u32) -> UVec2 {
    UVec2::new(checked.columns * (cell.x + gutter) + gutter, checked.rows * (cell.y + band + gutter) + gutter)
}

/// The height of the band a label takes under its picture on a sheet of `cell`-sized pictures. The type is set at
/// [`LABEL_DPI`] and grows with the page ([`Page::type_scale`]), and the page grows with the band, so it is worked
/// out until it holds; the scale is capped, so that takes two or three rounds.
fn label_band(checked: &Checked<'_>, cell: UVec2, gutter: u32) -> Result<u32, OpError> {
    let layout = |e: crate::sheet::layout::LayoutError| OpError::internal(format!("the sheet's labels: {e}"));
    let mut band = 0;
    for _ in 0..4 {
        let size = grid_size(checked, cell, gutter, band);
        let scale = Page::custom(size.x.min(MAX_CANVAS_SIDE), size.y.min(MAX_CANVAS_SIDE), LABEL_DPI)
            .map_err(layout)?
            .type_scale();
        // A one-pixel page at the scaled resolution sets type exactly as the sheet will, without its canvas.
        let probe = SheetLayout::new(Page::custom(1, 1, LABEL_DPI * scale).map_err(layout)?, Theme::default())
            .map_err(layout)?;
        let tall = 4096;
        let geometry = probe.cell_geometry(Rect::new(0, 0, 64, tall), &Cell::blank().caption("Ag"));
        let next = u32::try_from(tall - geometry.image.h).unwrap_or(0);
        if next == band {
            break;
        }
        band = band.max(next);
    }
    Ok(band)
}

/// `image` as an `image` buffer.
fn buffer(image: &Image) -> Result<RgbaImage, OpError> {
    RgbaImage::from_raw(image.size.x, image.size.y, straight_rgba(image))
        .ok_or_else(|| OpError::internal("a cell's pixels don't fill it"))
}

impl Op for ContactSheet {
    fn meta() -> OpMeta {
        OpMeta {
            name: "render.contact_sheet",
            summary: "Lay stored renders (their omuzaki://render/ URIs), or a character rendered in each of several \
                      expressions, variants or library poses, out in a labelled grid and store the sheet as a PNG; \
                      returns its URI.",
            kind: OpKind::Job,
            scope: Scope::Render,
            // It changes nothing in the document (the PNG goes to the content store), so its empty patch is undone
            // exactly and nothing enters the history.
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let checked = self.check()?;
        if let Some(character) = &self.character
            && ctx.doc.get(&omuzaki_char_api::paths::character(character)).is_none()
        {
            return Err(OpError::not_found(format!("there is no character '{character}'"))
                .hint("check the id; characters live under characters/<id>"));
        }
        let cost = match &checked.cells {
            Cells::Images(_) => CostHint { seconds: Some(0.3), gpu_mib: Some(0), usd: None },
            Cells::Rendered { character, varied } => {
                let size = self.check_render()?;
                self.sheet_size(&checked, size)?;
                let bytes = if let Varied::Variants(variants) = varied {
                    let mut most = 0;
                    for (i, variant) in variants.iter().enumerate() {
                        let bytes = character_bytes(ctx, character, Some(variant), self.quality)
                            .map_err(at(format!("variants/{i}")))?;
                        most = most.max(bytes);
                    }
                    most
                } else {
                    character_bytes(ctx, character, self.variant.as_ref(), self.quality)?
                };
                cost(size, u32::try_from(varied.len()).unwrap_or(u32::MAX), self.quality, bytes)
            }
        };
        Ok(Plan { cost: Some(cost), ..Plan::default() })
    }

    fn work(&self, job: &JobCtx<'_>) -> Result<Value, OpError> {
        let checked = self.check()?;
        let (pictures, labels) = match &checked.cells {
            Cells::Images(images) => {
                let mut pictures = Vec::with_capacity(images.len());
                let mut labels = Vec::with_capacity(images.len());
                for (i, image) in images.iter().enumerate() {
                    #[allow(clippy::cast_precision_loss, reason = "at most 64 pictures")]
                    job.checkpoint("reading", 0.8 * i as f32 / images.len() as f32)?;
                    pictures.push(load(job, &format!("images/{i}"), image)?);
                    labels.push(match self.titles.get(i) {
                        Some(title) => title.clone(),
                        None => hash_of(&format!("images/{i}"), image)?.to_string().chars().take(HASH_LABEL).collect(),
                    });
                }
                (pictures, labels)
            }
            Cells::Rendered { character, varied } => (self.rendered(job, character, varied)?, Self::labels_of(varied)),
        };
        let cell = pictures.iter().fold(UVec2::ZERO, |most, p| most.max(p.size));
        let (size, band) = self.sheet_size(&checked, cell)?;
        let g = self.gutter;
        let corner = |i: u32| {
            UVec2::new(g + (i % checked.columns) * (cell.x + g), g + (i / checked.columns) * (cell.y + band + g))
        };
        let sheet = if self.labels {
            let page = Page::custom(size.x, size.y, LABEL_DPI).map_err(|e| OpError::invalid("cell", e.to_string()))?;
            let mut layout = SheetLayout::new(page, Theme::default())
                .map_err(|e| OpError::internal(format!("the sheet's layout: {e}")))?;
            for ((i, picture), label) in (0_u32..).zip(&pictures).zip(&labels) {
                // The picture at its own size, centred in its cell; the label on the cell's band, so the labels of a
                // row line up whatever the pictures' sizes.
                let at = (corner(i) + (cell - picture.size) / 2).as_ivec2();
                let size = picture.size.as_ivec2();
                layout.place(Rect::new(at.x, at.y, size.x, size.y), &Cell::image(buffer(picture)?));
                let at = corner(i).as_ivec2();
                let whole = Rect::new(at.x, at.y, cell.x.cast_signed(), (cell.y + band).cast_signed());
                layout.place(whole, &Cell::blank().caption(label.clone()));
            }
            straight(size, layout.finish().into_raw())?
        } else {
            let mut sheet = canvas("cell", size)?;
            for (i, picture) in (0_u32..).zip(&pictures) {
                paste(&mut sheet, picture, corner(i) + (cell - picture.size) / 2);
            }
            sheet
        };
        let stored = store(job, &sheet)?;
        job.checkpoint("done", 1.0)?;
        let result = ContactSheetResult {
            uri: stored.uri,
            hash: stored.hash.to_string(),
            size: [size.x, size.y],
            columns: checked.columns,
            rows: checked.rows,
            cell: [cell.x, cell.y],
            cells: labels,
        };
        serde_json::to_value(result).map_err(|e| OpError::internal(format!("serialising the sheet: {e}")))
    }
}

impl ContactSheet {
    /// `character` as the job loads it, with `variant` applied.
    fn body(&self, job: &JobCtx<'_>, character: &CharId, variant: Option<&VariantId>) -> Result<Body, OpError> {
        let clock = SystemClock::default();
        let services = BodyServices::new(
            job.service::<dyn CharacterBuilder>()?,
            job.service::<BuildServices>()?,
            job.service::<dyn Store>()?,
            job.service::<Capabilities>().map_or(MtoonSupport::NONE, |c| c.mtoon),
            &clock,
        );
        job.read(|doc| load_body(doc, &services, character, variant, self.quality.detail()))
    }

    /// The front shot of `body` in the sheet's framing.
    fn shot(&self, body: &Body) -> Result<Shot, OpError> {
        let landmarks = body.landmarks().map_err(|e| OpError::invalid("framing", e.to_string()))?;
        #[allow(clippy::cast_precision_loss, reason = "sides are at most 1024")]
        let aspect = self.cell[0] as f32 / self.cell[1] as f32;
        let camera = frame(self.framing, &landmarks, &CameraDef::preset(self.framing), aspect)
            .map_err(|e| OpError::invalid("framing", e.to_string()))?;
        Ok(Shot::new(camera))
    }

    /// One straight-alpha picture of `body` in `pose` for each of `shots`.
    fn render(
        &self,
        job: &JobCtx<'_>,
        body: Body,
        shots: &[Shot],
        pose: Option<&HumanoidPose>,
    ) -> Result<Vec<Image>, OpError> {
        let size = self.check_render()?;
        let spec = StillSpec::new(still_light(&self.light_rig)?, size, self.quality.quality()).with_pose(pose.copied());
        let stills = with_renderer(job, |renderer| render_body_stills(renderer, body, &spec, shots))?;
        stills.into_iter().map(|still| straight(still.image.size, straight_rgba(&still.image))).collect()
    }

    /// The front shot of `body`, showing the sheet's `expression` if it has one.
    fn shot_with_expression(&self, body: &Body) -> Result<Shot, OpError> {
        let shot = self.shot(body)?;
        let Some(expression) = &self.expression else { return Ok(shot) };
        check_expression(body, expression)?;
        Ok(shot.with_expression(expression.clone(), 1.0))
    }

    /// The sheet's shared `pose` as `body` shows it.
    fn shared_pose(&self, body: &Body) -> Result<Option<HumanoidPose>, OpError> {
        self.pose.as_ref().map(|pose| pose.resolve("pose", body)).transpose()
    }

    /// The rendered cells, in order: the expressions from one staging; a staging for each variant or pose.
    fn rendered(&self, job: &JobCtx<'_>, character: &CharId, varied: &Varied<'_>) -> Result<Vec<Image>, OpError> {
        #[allow(clippy::cast_precision_loss, reason = "at most 64 cells")]
        let progress = |i: usize| job.checkpoint("rendering", 0.9 * i as f32 / varied.len() as f32);
        match varied {
            Varied::Expressions(expressions) => {
                let body = self.body(job, character, self.variant.as_ref())?;
                for (i, expression) in expressions.iter().enumerate() {
                    check_expression(&body, expression).map_err(at(format!("expressions/{i}")))?;
                }
                let shot = self.shot(&body)?;
                let shots: Vec<Shot> =
                    expressions.iter().map(|e| shot.clone().with_expression(e.clone(), 1.0)).collect();
                let pose = self.shared_pose(&body)?;
                progress(0)?;
                self.render(job, body, &shots, pose.as_ref())
            }
            Varied::Variants(variants) => {
                let mut pictures = Vec::with_capacity(variants.len());
                for (i, variant) in variants.iter().enumerate() {
                    progress(i)?;
                    let body = self.body(job, character, Some(variant)).map_err(at(format!("variants/{i}")))?;
                    let (shot, pose) = (self.shot_with_expression(&body)?, self.shared_pose(&body)?);
                    pictures.extend(self.render(job, body, &[shot], pose.as_ref())?);
                }
                Ok(pictures)
            }
            Varied::Poses(poses) => {
                let mut pictures = Vec::with_capacity(poses.len());
                for (i, pose) in poses.iter().enumerate() {
                    progress(i)?;
                    let body = self.body(job, character, self.variant.as_ref())?;
                    let shot = self.shot_with_expression(&body)?;
                    let pose = pose.resolve(&format!("poses/{i}"), &body)?;
                    pictures.extend(self.render(job, body, &[shot], Some(&pose))?);
                }
                Ok(pictures)
            }
        }
    }

    /// Each rendered cell's label.
    fn labels_of(varied: &Varied<'_>) -> Vec<String> {
        match varied {
            Varied::Expressions(list) => list.iter().map(ToString::to_string).collect(),
            Varied::Variants(list) => list.iter().map(ToString::to_string).collect(),
            Varied::Poses(list) => list.iter().map(PoseArg::label).collect(),
        }
    }
}

register_op!(ContactSheet);

#[cfg(test)]
mod tests {
    use super::*;

    fn sheet(n: usize, columns: Option<u32>) -> ContactSheet {
        let hash = omuzaki_store_api::ContentHash::of(b"x").to_string();
        serde_json::from_value(serde_json::json!({ "images": vec![hash; n], "columns": columns })).expect("reads")
    }

    fn grid(sheet: &ContactSheet) -> (u32, u32) {
        let checked = sheet.check().expect("checks");
        (checked.columns, checked.rows)
    }

    #[test]
    fn the_grid_is_as_square_as_the_count_allows_unless_columns_are_given() {
        assert_eq!(grid(&sheet(1, None)), (1, 1));
        assert_eq!(grid(&sheet(4, None)), (2, 2));
        assert_eq!(grid(&sheet(5, None)), (3, 2));
        assert_eq!(grid(&sheet(5, Some(5))), (5, 1));
        assert_eq!(grid(&sheet(3, Some(10))), (3, 1), "no empty columns");
    }

    #[test]
    fn an_empty_or_oversized_sheet_is_refused() {
        assert!(sheet(0, None).check().is_err());
        assert!(sheet(MAX_IMAGES + 1, None).check().is_err());
        assert!(sheet(2, Some(0)).check().is_err());
        assert!(sheet(2, Some(MAX_COLUMNS + 1)).check().is_err());
        assert!(ContactSheet { gutter: MAX_GUTTER + 1, ..sheet(1, None) }.check().is_err());
        assert!(ContactSheet { images: vec!["not a render".into()], ..sheet(1, None) }.check().is_err());
    }

    #[test]
    fn titles_name_stored_images_one_each() {
        let titled = |titles: &[&str]| ContactSheet {
            titles: titles.iter().map(|t| (*t).to_owned()).collect(),
            ..sheet(2, None)
        };
        assert!(titled(&["studio", "stage"]).check().is_ok());
        assert!(titled(&["studio"]).check().is_err(), "one short");
        assert!(titled(&["studio", &"x".repeat(MAX_TITLE + 1)]).check().is_err(), "too long");
        let rendered: ContactSheet = serde_json::from_value(
            serde_json::json!({ "character": "kit", "variants": ["close-up"], "titles": ["a"] }),
        )
        .expect("reads");
        assert!(rendered.check().is_err(), "a rendered cell is labelled by what it shows");
    }

    #[test]
    fn the_label_band_holds_a_line_of_type_and_grows_with_the_page() {
        let small = sheet(4, None);
        let checked = small.check().expect("checks");
        let band = label_band(&checked, UVec2::new(64, 64), 8).expect("a band");
        assert!((20..=48).contains(&band), "a 9 pt line at 144 dpi: {band}");
        let large = label_band(&checked, UVec2::new(1024, 1024), 8).expect("a band");
        assert!(large > band, "a larger page sets larger type: {large} vs {band}");
    }
}
