//! Cloth on garments (#387): a garment's lofted grid simulated as a sheet ([`ClothRuntime`]).
//!
//! A garment that drapes (a skirt, a dress, a robe, a cloak) builds its mesh as it always does, skinned to the
//! skeleton. When its [`ClothSpec`] is enabled, [`simulate`] turns that mesh into the runtime's: the garment names the
//! grid of its main surface ([`Drape`]: where it starts in the mesh, its rows and columns, and which edge is pinned),
//! and every other vertex of the mesh (the lining, a rolled hem, the repeated seam column) rides on the grid vertex
//! nearest it ([`MeshVertex`]). The sheet collides with the body under the garment as the fit stack gives it (so a
//! robe keeps off the skirt under it as well as the legs) and with the hair's collider proxies. The mesh becomes
//! dynamic: the runtime draws it every frame, in character space.
//!
//! An export build (`rig_export`) has no solver to run on: the sheet is baked to spring-bone chains
//! ([`bake`](crate::cloth::bake)) and the mesh skinned to them, every rider with its host's weights.
//!
//! **Presets.** The fabric is one of four presets (`library/cloth-presets/*.toml`, data files): `light` (chiffon),
//! `cotton`, `heavy` (wool) and `leather`, each a full set of the solver's numbers, with a few overrides on top.
//!
//! **Drag by length.** A garment that hangs further below its pins than the kit's skirt takes extra air drag
//! ([`hang_drag`]), so a dress, a robe or a cloak settles as fast as the skirt does (0.8 s, #387) instead of creeping
//! for up to 2 s, while the swing after a turn stays. A damping override is taken as given.

use std::sync::OnceLock;

use omuzaki_char_api::{BuildCtx, BuildError, JointRef, PartOutput};
use omuzaki_core::glam::Vec3;
use omuzaki_face_api::{Check, Validate};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::cloth::bake::{BakeTarget, bake};
use crate::cloth::collide::ColliderSet;
use crate::cloth::contact::{ContactParams, Friction};
use crate::cloth::runtime::{self, ClothGrid, ClothRuntime, MeshVertex, SheetSize, SkinWeights};
use crate::cloth::solver::ClothParams;

/// The most bones a baked garment may add to an export.
pub const BAKE_BONES: usize = 48;
/// How close two vertices must be to count as one point (a seam's twin), metres.
const SAME_POINT: f32 = 1e-6;
/// The hang below which a garment takes no extra drag: about the kit skirt's (14 cm), which settles in time unaided.
const DRAG_FROM_M: f32 = 0.14;
/// How much further a garment hangs before it takes the whole of [`DRAG_MAX`].
const DRAG_OVER_M: f32 = 0.16;
/// The most extra drag, 1/s. Measured on the kit (`tests/cloth_garments.rs`): with it every garment in every preset
/// settles in 0.75 s or less and comes back after a turn. Twice it leaves a chiffon robe hanging behind the turn, and a
/// chiffon cloak creeping for a second.
const DRAG_MAX: f32 = 10.0;
/// The sheets a garment is simulated on, finest first, before they are cut to its grid.
const LEVELS: [SheetSize; 3] = [SheetSize::new(24, 16), SheetSize::new(12, 8), SheetSize::new(8, 6)];

/// The fabric a garment's cloth is made of.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ClothPreset {
    /// Chiffon and silk: light, floaty, slow to settle.
    Light,
    /// Medium cotton: the default.
    #[default]
    Cotton,
    /// Wool: heavy, swings slowly and hangs straight.
    Heavy,
    /// Leather: stiff, moves as a piece.
    Leather,
}

impl ClothPreset {
    /// Every preset, in order.
    pub const ALL: [Self; 4] = [Self::Light, Self::Cotton, Self::Heavy, Self::Leather];

    /// Its name, as its data file is named.
    pub fn name(self) -> &'static str {
        match self {
            Self::Light => "light",
            Self::Cotton => "cotton",
            Self::Heavy => "heavy",
            Self::Leather => "leather",
        }
    }

    /// Its numbers.
    pub fn fabric(self) -> &'static Fabric {
        let all = presets();
        match self {
            Self::Light => &all[0],
            Self::Cotton => &all[1],
            Self::Heavy => &all[2],
            Self::Leather => &all[3],
        }
    }
}

/// A fabric's numbers, as a preset's data file gives them.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Fabric {
    /// Mass per area, kg/m².
    pub density: f32,
    /// Stretch compliance, m/N.
    pub stretch: f32,
    /// Shear compliance, m/N.
    pub shear: f32,
    /// Bend compliance, 1/(N·m).
    pub bend: f32,
    /// Velocity decay, 1/s.
    pub damping: f32,
    /// Half the cloth's thickness, metres.
    pub thickness: f32,
    /// The stretch limit, as a share of an edge's rest length.
    pub max_stretch: f32,
    /// Static and kinetic friction against the body.
    pub friction: [f32; 2],
    /// How strongly the cloth holds its designed shape: the soft pin weight of every free particle (see
    /// [`runtime::ClothSpec::shape`]).
    pub shape: f32,
}

/// The four presets, read once from their data files.
fn presets() -> &'static [Fabric; 4] {
    static PRESETS: OnceLock<[Fabric; 4]> = OnceLock::new();
    PRESETS.get_or_init(|| {
        let read = |text: &str| toml::from_str::<Fabric>(text).unwrap_or_else(|_| Fabric::fallback());
        [
            read(include_str!("../../library/cloth-presets/light.toml")),
            read(include_str!("../../library/cloth-presets/cotton.toml")),
            read(include_str!("../../library/cloth-presets/heavy.toml")),
            read(include_str!("../../library/cloth-presets/leather.toml")),
        ]
    })
}

impl Fabric {
    /// The solver's default cotton, should a data file not read (a test checks every one does).
    fn fallback() -> Self {
        let p = ClothParams::default();
        let c = ContactParams::default();
        Self {
            density: 0.2,
            stretch: p.stretch,
            shear: p.shear,
            bend: p.bend,
            damping: p.damping,
            thickness: p.thickness,
            max_stretch: p.max_stretch,
            friction: [c.friction.static_, c.friction.kinetic],
            shape: 0.0,
        }
    }

    /// Whether the data file of `preset` reads (for tests).
    pub fn reads(preset: ClothPreset) -> bool {
        let text = match preset {
            ClothPreset::Light => include_str!("../../library/cloth-presets/light.toml"),
            ClothPreset::Cotton => include_str!("../../library/cloth-presets/cotton.toml"),
            ClothPreset::Heavy => include_str!("../../library/cloth-presets/heavy.toml"),
            ClothPreset::Leather => include_str!("../../library/cloth-presets/leather.toml"),
        };
        toml::from_str::<Fabric>(text).is_ok()
    }
}

/// The few numbers of a preset a garment may change.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct ClothOverrides {
    /// Mass per area, kg/m² (0.02 to 2).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub density: Option<f64>,
    /// How much stiffer in bending than the preset, as a factor (0.05 to 20): above 1 hangs stiffer and swings less.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stiffness: Option<f64>,
    /// How fast motion dies away, 1/s (0 to 20).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub damping: Option<f64>,
}

impl Validate for ClothOverrides {
    fn check(&self, c: &mut Check) {
        if let Some(v) = self.density {
            c.range("density", v, 0.02, 2.0);
        }
        if let Some(v) = self.stiffness {
            c.range("stiffness", v, 0.05, 20.0);
        }
        if let Some(v) = self.damping {
            c.range("damping", v, 0.0, 20.0);
        }
    }
}

/// Cloth simulation for a garment: off (the garment is skinned to the skeleton) or a sheet of a preset fabric.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields, default)]
pub struct ClothSpec {
    /// Whether the garment is simulated. A garment that cannot drape (a vest, boots) ignores it.
    pub enabled: bool,
    /// The fabric.
    pub preset: ClothPreset,
    /// Changes to the preset's numbers.
    pub overrides: ClothOverrides,
}

impl Validate for ClothSpec {
    fn check(&self, c: &mut Check) {
        c.nested("overrides", &self.overrides);
    }
}

impl ClothSpec {
    /// The solver's numbers for this fabric: the preset's, with the overrides.
    #[allow(clippy::cast_possible_truncation)] // the solver is f32
    pub fn params(&self) -> (ClothParams, ContactParams, f32, f32) {
        let f = self.preset.fabric();
        let stiffness = self.overrides.stiffness.unwrap_or(1.0) as f32;
        let params = ClothParams {
            stretch: f.stretch,
            shear: f.shear,
            bend: f.bend / stiffness,
            damping: self.overrides.damping.map_or(f.damping, |d| d as f32),
            thickness: f.thickness,
            max_stretch: f.max_stretch,
            ..ClothParams::default()
        };
        let contact = ContactParams {
            thickness: f.thickness,
            friction: Friction { static_: f.friction[0], kinetic: f.friction[1] },
            ..ContactParams::default()
        };
        (params, contact, self.overrides.density.map_or(f.density, |d| d as f32), f.shape)
    }
}

/// Where a garment's main surface lies in its mesh: the grid the sheet is cut from.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Drape {
    /// The mesh, by its index in the part's output.
    pub mesh: usize,
    /// The grid's first vertex in the mesh.
    pub start: usize,
    /// Rows of the grid.
    pub rows: usize,
    /// Columns of the grid, a seam's repeated column included.
    pub cols: usize,
    /// Whether row 0 is the pinned edge (a waistband, a yoke); otherwise the last row is.
    pub pinned_first: bool,
    /// How many grid rows from the pinned edge stay on the body.
    pub pin_rows: usize,
}

/// Simulates the garment's mesh `drape.mesh` of `out` as `spec` asks: replaces its skin with a [`ClothRuntime`] (or, for
/// an export, the baked spring chains). Does nothing when `spec` is off.
///
/// # Errors
/// [`BuildError::Part`] for a drape that doesn't fit the mesh or a sheet the runtime refuses.
pub fn simulate(spec: &ClothSpec, drape: &Drape, ctx: &BuildCtx<'_>, out: &mut PartOutput) -> Result<(), BuildError> {
    if !spec.enabled {
        return Ok(());
    }
    let fail = |m: String| ctx.fail(format!("cloth: {m}"));
    let mesh = out.meshes.get(drape.mesh).ok_or_else(|| fail("the garment has no such mesh".to_owned()))?;
    let (grid, layout) = grid_of(drape, &mesh.geometry, &mesh.joint_bones).map_err(fail)?;
    let (mut params, contact, density, shape) = spec.params();
    if spec.overrides.damping.is_none() {
        params.damping += hang_drag(hang_of(&grid));
    }
    let fit = ctx.fit()?;
    let mut colliders = ColliderSet::new();
    colliders.add_body(fit.as_ref());
    colliders.add_hair(ctx.colliders(), fit.as_ref());
    let levels = levels(grid.rows, grid.cols);
    let pin_rows = sheet_pins(drape.pin_rows, grid.rows, levels[0].rows);
    let cloth = runtime::ClothSpec {
        levels: levels.clone(),
        pin_rows,
        density,
        params,
        contact,
        quality: ctx.quality,
        lod: lod(levels.len()),
        mesh: u16::try_from(drape.mesh).map_err(|_| fail("too many meshes".to_owned()))?,
        shape,
        ..runtime::ClothSpec::default()
    };
    let rt = ClothRuntime::new(&grid, &cloth, colliders).map_err(|e| fail(e.to_string()))?;
    let mesh = &mut out.meshes[drape.mesh];
    if ctx.rig_export {
        let baked = bake(&rt, ctx.part.as_str(), BAKE_BONES, fit.as_ref()).map_err(|e| fail(e.to_string()))?;
        let names: Vec<JointRef> = baked.rig.bones.iter().map(|b| JointRef::Named(b.name.clone())).collect::<Vec<_>>();
        let mut joint_bones: Vec<JointRef> = Vec::new();
        let mut index_of = |joint: JointRef| -> u16 {
            if let Some(k) = joint_bones.iter().position(|j| *j == joint) {
                return u16::try_from(k).unwrap_or(0);
            }
            joint_bones.push(joint);
            u16::try_from(joint_bones.len() - 1).unwrap_or(0)
        };
        let mut joints = Vec::with_capacity(layout.len());
        let mut weights = Vec::with_capacity(layout.len());
        for vertex in &layout {
            let g = match *vertex {
                MeshVertex::Grid(k) | MeshVertex::Twin(k) | MeshVertex::Rides { host: k, .. } => k as usize,
            };
            let (mut j4, mut w4) = ([0_u16; 4], [0.0_f32; 4]);
            for (slot, (target, w)) in baked.weights[g].iter().take(4).enumerate() {
                let joint = match target {
                    BakeTarget::Joint(joint) => joint.clone(),
                    BakeTarget::Bone(id) => names.get(id.0 as usize).cloned().unwrap_or(JointRef::Named(
                        omuzaki_char_api::BoneName::new("root").map_err(|_| fail("bone name".to_owned()))?,
                    )),
                };
                j4[slot] = index_of(joint);
                w4[slot] = *w;
            }
            joints.push(j4);
            weights.push(w4);
        }
        mesh.geometry.joints = joints;
        mesh.geometry.weights = weights;
        mesh.joint_bones = joint_bones;
        if !out.rig.is_empty() {
            return Err(fail("a garment with cloth adds nothing else to the rig".to_owned()));
        }
        out.rig = baked.rig;
        out.warnings.extend(baked.warnings);
    } else {
        mesh.geometry.joints.clear();
        mesh.geometry.weights.clear();
        mesh.joint_bones.clear();
        mesh.dynamic = true;
        out.runtime = Some(Box::new(rt));
    }
    Ok(())
}

/// How far `grid` hangs, metres: from the highest point of its pinned row to its lowest point.
fn hang_of(grid: &ClothGrid) -> f32 {
    let top = grid.positions.iter().take(grid.cols).map(|p| p.y).fold(f32::MIN, f32::max);
    let bottom = grid.positions.iter().map(|p| p.y).fold(f32::MAX, f32::min);
    (top - bottom).max(0.0)
}

/// The extra air drag, 1/s, of a garment that hangs `hang` metres: none up to `DRAG_FROM_M`, rising evenly to
/// `DRAG_MAX` over `DRAG_OVER_M` more. A long sheet held only at its top edge creeps toward its hang slowly; the
/// drag lets it get there in the time a skirt does.
pub fn hang_drag(hang: f32) -> f32 {
    DRAG_MAX * ((hang - DRAG_FROM_M) / DRAG_OVER_M).clamp(0.0, 1.0)
}

/// The sheet levels for a grid of `rows x cols` (a wrapped grid's columns without the twin): [`LEVELS`] cut to the grid,
/// with any level no coarser than the one before dropped.
fn levels(rows: usize, cols: usize) -> Vec<SheetSize> {
    let mut out: Vec<SheetSize> = Vec::new();
    for size in LEVELS {
        let size = SheetSize::new(size.cols.min(cols), size.rows.min(rows));
        if out.last().is_none_or(|last| size.cols * size.rows < last.cols * last.rows) {
            out.push(size);
        }
    }
    out
}

/// The level-of-detail thresholds for `levels` levels: the runtime's defaults, as many as there are levels.
fn lod(levels: usize) -> runtime::LodPolicy {
    let default = runtime::LodPolicy::default();
    runtime::LodPolicy { level_px: default.level_px.into_iter().take(levels.saturating_sub(1)).collect(), ..default }
}

/// The sheet rows pinned when `grid_pins` of `grid_rows` grid rows stay on the body, for a sheet of `sheet_rows`.
fn sheet_pins(grid_pins: usize, grid_rows: usize, sheet_rows: usize) -> usize {
    if grid_rows < 2 {
        return 1;
    }
    // The sheet rows are picked evenly from the grid's; count those on a pinned grid row.
    let last_pinned = grid_pins.max(1) - 1;
    (0..sheet_rows)
        .filter(|&j| (j * (grid_rows - 1) * 2 + (sheet_rows - 1)) / (2 * (sheet_rows - 1)) <= last_pinned)
        .count()
        .clamp(1, sheet_rows - 1)
}

/// The runtime's grid for `drape` over `geometry`, and the drawn mesh's layout: grid vertices in the drape's order
/// (the pinned row first, the seam's twin column left out of a wrapped grid), and every other vertex riding on the
/// nearest grid vertex.
///
/// # Errors
/// A message for a drape that runs past the mesh, a mesh without skinning, or a grid too small to simulate.
fn grid_of(
    drape: &Drape,
    geometry: &omuzaki_core::mesh::MeshData,
    joint_bones: &[JointRef],
) -> Result<(ClothGrid, Vec<MeshVertex>), String> {
    let (rows, cols) = (drape.rows, drape.cols);
    let n = geometry.positions.len();
    if rows < 2 || cols < 3 || drape.start + rows * cols > n {
        return Err(format!("a {rows} x {cols} grid from vertex {} doesn't fit a mesh of {n}", drape.start));
    }
    if !geometry.is_skinned() || joint_bones.is_empty() {
        return Err("the garment's mesh isn't skinned".to_owned());
    }
    let mesh_index = |r: usize, c: usize| {
        let row = if drape.pinned_first { r } else { rows - 1 - r };
        drape.start + row * cols + c
    };
    let seam = (0..rows).all(|r| {
        geometry.positions[mesh_index(r, 0)].distance(geometry.positions[mesh_index(r, cols - 1)]) <= SAME_POINT
    });
    let grid_cols = if seam { cols - 1 } else { cols };
    let skin_of = |m: usize| SkinWeights { joints: geometry.joints[m], weights: geometry.weights[m] };
    let mut positions = Vec::with_capacity(rows * grid_cols);
    let mut skin = Vec::with_capacity(rows * grid_cols);
    let mut layout: Vec<Option<MeshVertex>> = vec![None; n];
    for r in 0..rows {
        for c in 0..cols {
            let m = mesh_index(r, c);
            if seam && c == cols - 1 {
                layout[m] = Some(MeshVertex::Twin(index_u32(r * grid_cols)?));
                continue;
            }
            layout[m] = Some(MeshVertex::Grid(index_u32(positions.len())?));
            positions.push(geometry.positions[m]);
            skin.push(skin_of(m));
        }
    }
    let hosts = Hosts::new(&positions);
    let layout = layout
        .into_iter()
        .enumerate()
        .map(|(m, v)| {
            if let Some(v) = v {
                return Ok(v);
            }
            let p = geometry.positions[m];
            let (host, distance) = hosts.nearest(p);
            let host = index_u32(host)?;
            Ok(if distance <= SAME_POINT {
                MeshVertex::Twin(host)
            } else {
                MeshVertex::Rides { host, rest: p, normal: geometry.normals[m], skin: skin_of(m) }
            })
        })
        .collect::<Result<Vec<_>, String>>()?;
    let grid = ClothGrid {
        rows,
        cols: grid_cols,
        wrap: seam,
        positions,
        skin,
        joints: joint_bones.to_vec(),
        mesh: layout.clone(),
    };
    Ok((grid, layout))
}

/// `k` as a vertex index.
fn index_u32(k: usize) -> Result<u32, String> {
    u32::try_from(k).map_err(|_| "too many vertices".to_owned())
}

/// The grid's vertices bucketed in cells, for finding the one nearest a point.
struct Hosts<'a> {
    points: &'a [Vec3],
    cell: f32,
    cells: std::collections::HashMap<(i32, i32, i32), Vec<usize>>,
}

impl<'a> Hosts<'a> {
    /// Buckets `points` in cells about as big as the mean spacing of a grid of that many points over their bounds.
    #[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)] // vertex counts; cell coordinates
    fn new(points: &'a [Vec3]) -> Self {
        let (lo, hi) =
            points.iter().fold((Vec3::splat(f32::MAX), Vec3::splat(f32::MIN)), |(lo, hi), p| (lo.min(*p), hi.max(*p)));
        let extent = (hi - lo).max_element().max(1e-3);
        let cell = (extent / (points.len() as f32).sqrt().max(1.0)).max(1e-4) * 2.0;
        let mut cells: std::collections::HashMap<(i32, i32, i32), Vec<usize>> = std::collections::HashMap::new();
        for (k, p) in points.iter().enumerate() {
            cells.entry(Self::key_of(*p, cell)).or_default().push(k);
        }
        Self { points, cell, cells }
    }

    #[allow(clippy::cast_possible_truncation)] // cell coordinates of a garment are small
    fn key_of(p: Vec3, cell: f32) -> (i32, i32, i32) {
        let q = (p / cell).floor();
        (q.x as i32, q.y as i32, q.z as i32)
    }

    /// The point nearest `p` and its distance: searched in growing shells of cells until no nearer one can be outside.
    #[allow(clippy::cast_precision_loss)] // shell radii
    fn nearest(&self, p: Vec3) -> (usize, f32) {
        let (cx, cy, cz) = Self::key_of(p, self.cell);
        let mut best = (0, f32::INFINITY);
        for ring in 0..64_i32 {
            for dx in -ring..=ring {
                for dy in -ring..=ring {
                    for dz in -ring..=ring {
                        if dx.abs().max(dy.abs()).max(dz.abs()) != ring {
                            continue;
                        }
                        if let Some(list) = self.cells.get(&(cx + dx, cy + dy, cz + dz)) {
                            for &k in list {
                                let d = self.points[k].distance(p);
                                if (d, k) < (best.1, best.0) {
                                    best = (k, d);
                                }
                            }
                        }
                    }
                }
            }
            // Every point outside the shells searched is at least `ring` cells away.
            if best.1 <= ring as f32 * self.cell {
                return best;
            }
        }
        if best.1.is_finite() {
            return best;
        }
        self.points
            .iter()
            .enumerate()
            .map(|(k, q)| (k, q.distance(p)))
            .fold((0, f32::INFINITY), |a, b| if b.1 < a.1 { b } else { a })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_preset_reads_and_they_differ() {
        for preset in ClothPreset::ALL {
            assert!(Fabric::reads(preset), "{}", preset.name());
        }
        let bends: Vec<f32> = ClothPreset::ALL.iter().map(|p| p.fabric().bend).collect();
        assert!(bends.windows(2).all(|w| w[0] > w[1]), "each preset stiffer in bending than the one before: {bends:?}");
    }

    #[test]
    fn levels_are_cut_to_the_grid_and_keep_getting_coarser() {
        assert_eq!(levels(12, 144), vec![SheetSize::new(24, 12), SheetSize::new(12, 8), SheetSize::new(8, 6)]);
        assert_eq!(levels(5, 10), vec![SheetSize::new(10, 5), SheetSize::new(8, 5)]);
        assert_eq!(lod(2).level_px.len(), 1);
    }

    #[test]
    fn the_drag_grows_with_the_hang_and_stops_growing() {
        assert!(hang_drag(0.1).abs() < f32::EPSILON && hang_drag(DRAG_FROM_M).abs() < f32::EPSILON);
        assert!((hang_drag(DRAG_FROM_M + DRAG_OVER_M / 2.0) - DRAG_MAX / 2.0).abs() < 1e-4);
        assert!((hang_drag(1.0) - DRAG_MAX).abs() < f32::EPSILON);
    }

    #[test]
    fn the_pinned_sheet_rows_follow_the_grid_rows() {
        assert_eq!(sheet_pins(1, 12, 12), 1);
        assert_eq!(sheet_pins(3, 12, 12), 3);
        assert_eq!(sheet_pins(12, 12, 8), 7);
    }

    #[test]
    fn the_nearest_host_is_found_in_any_cell() {
        let points: Vec<Vec3> =
            (0..200_u16).map(|k| Vec3::new(f32::from(k % 20) * 0.01, f32::from(k / 20) * 0.01, 0.0)).collect();
        let hosts = Hosts::new(&points);
        for (k, q) in
            [(0, Vec3::new(-0.5, -0.5, 0.3)), (199, Vec3::new(0.19, 0.09, 0.001)), (45, Vec3::new(0.05, 0.02, 0.0))]
        {
            assert_eq!(hosts.nearest(q).0, k);
        }
    }
}
