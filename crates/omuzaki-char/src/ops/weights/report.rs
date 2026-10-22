//! `rig.weights.report`: how a part's skin came out after its brushes, and which brushes need a look.

use std::collections::BTreeSet;

use omuzaki_char_api::{CharId, JointRef, PartId, PartMesh, WeightBrush};
use omuzaki_core::glam::Vec3;
use omuzaki_face_api::narrow;
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{brushes_of, build_character, straddling};
use crate::kit::skin::{MAX_INFLUENCES, max_neighbour_jump};
use crate::ops::character::access;

/// How far a vertex's weights may sum from 1 and still count as normalised (float rounding).
const NORMALISED_TOLERANCE: f32 = 1e-4;

/// Reports on a part's skin after its weight brushes: a query, so it changes nothing.
///
/// The answer has `vertices` (of the part's skinned meshes) and four checks on the built skin. `unnormalised` counts
/// vertices whose weights do not sum to 1, and `malformed` counts vertices with a joint index outside the mesh's joint
/// list, a joint named twice, or a weight that is not a finite number. Both are 0 after a correct build. The third,
/// `over_four_influences`, counts vertices whose brushes name more than four joints in all, counting the generator's
/// skin: the build keeps the four heaviest, so this is where weight is trimmed. `max_neighbour_jump` is the largest
/// weight change across one mesh edge: a seam that is too abrupt shows here.
///
/// Per brush, in order, `brushes` gives how many vertices it `reaches` (lies inside its radius), and
/// `brushes_without_vertices` lists the brushes that reach none, so a brush that misses the mesh is found before anyone
/// wonders why it does nothing. `straddling` lists the brushes that cross the centre plane with a different mirror
/// image (see `rig.weights.mirror`). A disabled part is not built, so its report says so and counts nothing.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [
        {"character": "kit", "part": "body"}
    ],
    "x-omuzaki-fixture" = "character-brush"
))]
pub struct WeightsReport {
    /// The character the part belongs to.
    pub character: CharId,
    /// The part to report on. Must exist.
    pub part: PartId,
}

impl Op for WeightsReport {
    fn meta() -> OpMeta {
        OpMeta {
            name: "rig.weights.report",
            summary: "Report a part's skin weights: unnormalised or over-influenced vertices, the worst seam, and brushes that reach nothing or cross the plane.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let def = access::load(ctx, &self.character)?;
        let enabled = access::part(&def, &self.character, &self.part, "part")?.enabled;
        let brushes = brushes_of(&def, &self.part);
        let built = build_character(&self.character, &def)?;
        // The skin the generator gives, without this part's brushes: the base for the influences before the limit.
        let mut unbrushed = def.clone();
        unbrushed.weight_brushes.remove(&self.part);
        let base = build_character(&self.character, &unbrushed)?;
        let brushed_meshes = skinned_meshes(&built, &self.part);
        let base_meshes = skinned_meshes(&base, &self.part);
        let pairs: Vec<(&PartMesh, &PartMesh)> = brushed_meshes.into_iter().zip(base_meshes).collect();
        let answer = Answer { character: &self.character, part: &self.part, enabled, brushes: &brushes, pairs: &pairs };
        Ok(Plan::answer(answer.json()))
    }
}

/// The skinned meshes `part` built, in build order.
fn skinned_meshes<'a>(built: &'a omuzaki_char_api::BuiltCharacter, part: &PartId) -> Vec<&'a PartMesh> {
    built.meshes.iter().filter(|m| m.part == *part && m.mesh.geometry.is_skinned()).map(|m| &m.mesh).collect()
}

/// What the report measures, gathered for one part.
struct Answer<'a> {
    character: &'a CharId,
    part: &'a PartId,
    enabled: bool,
    brushes: &'a [WeightBrush],
    /// Each skinned mesh with its brushes applied, paired with the same mesh without them.
    pairs: &'a [(&'a PartMesh, &'a PartMesh)],
}

impl Answer<'_> {
    /// The report's JSON.
    fn json(&self) -> Value {
        let mut vertices = 0_usize;
        let mut over_four = 0_usize;
        let mut unnormalised = 0_usize;
        let mut malformed = 0_usize;
        let mut jump = 0.0_f32;
        for (brushed, base) in self.pairs {
            vertices += brushed.geometry.positions.len();
            over_four += influences_over_limit(base, self.brushes);
            unnormalised += unnormalised_count(brushed);
            malformed += malformed_count(brushed);
            jump = jump.max(max_neighbour_jump(brushed));
        }
        let meshes: Vec<&PartMesh> = self.pairs.iter().map(|(brushed, _)| *brushed).collect();
        let reaches: Vec<usize> = self.brushes.iter().map(|b| meshes.iter().map(|m| in_reach(m, b)).sum()).collect();
        let without: Vec<usize> = reaches.iter().enumerate().filter(|(_, n)| **n == 0).map(|(i, _)| i).collect();
        let per_brush: Vec<Value> =
            reaches.iter().enumerate().map(|(index, n)| json!({"index": index, "reaches": n})).collect();
        let mut out = json!({
            "character": self.character,
            "part": self.part,
            "enabled": self.enabled,
            "vertices": vertices,
            "over_four_influences": over_four,
            "unnormalised": unnormalised,
            "malformed": malformed,
            "max_neighbour_jump": f64::from(jump),
            "brushes": per_brush,
            "brushes_without_vertices": without,
            "straddling": straddling(self.brushes),
        });
        if !self.enabled {
            out["note"] = json!("the part is disabled, so it is not built: enable it to report its skin");
        }
        out
    }
}

/// The vertices of `base` whose brushes name more than [`MAX_INFLUENCES`] joints in all, before the build keeps only the
/// four heaviest. A vertex's joints are the ones its generated skin gives weight to, plus each brush's joint when the
/// brush reaches the vertex (the same test `apply_brushes` makes).
fn influences_over_limit(base: &PartMesh, brushes: &[WeightBrush]) -> usize {
    (0..base.geometry.positions.len())
        .filter(|&vertex| {
            let mut joints: BTreeSet<JointRef> = live_joints(base, vertex).into_iter().collect();
            let position = base.geometry.positions[vertex];
            for brush in brushes.iter().filter(|b| reaches_point(position, b)) {
                joints.insert(brush.joint.clone());
            }
            joints.len() > MAX_INFLUENCES
        })
        .count()
}

/// The joints a vertex's skin gives weight to, by name.
fn live_joints(mesh: &PartMesh, vertex: usize) -> Vec<JointRef> {
    let (joints, weights) = (mesh.geometry.joints[vertex], mesh.geometry.weights[vertex]);
    joints
        .iter()
        .zip(weights)
        .filter(|(_, w)| *w > 0.0)
        .filter_map(|(j, _)| mesh.joint_bones.get(usize::from(*j)).cloned())
        .collect()
}

/// The vertices whose weights do not sum to 1 within [`NORMALISED_TOLERANCE`].
fn unnormalised_count(mesh: &PartMesh) -> usize {
    mesh.geometry
        .weights
        .iter()
        .filter(|weights| (weights.iter().sum::<f32>() - 1.0).abs() > NORMALISED_TOLERANCE)
        .count()
}

/// The vertices with a live slot that names a joint outside the mesh's joint list, a joint named in two live slots, or a
/// weight that is not finite. A slot with weight 0 is unused, so its index is ignored.
fn malformed_count(mesh: &PartMesh) -> usize {
    let joints_known = mesh.joint_bones.len();
    (0..mesh.geometry.weights.len())
        .filter(|&vertex| {
            let (joints, weights) = (mesh.geometry.joints[vertex], mesh.geometry.weights[vertex]);
            let live: Vec<(u16, f32)> =
                joints.iter().zip(weights).filter(|(_, w)| *w != 0.0).map(|(j, w)| (*j, w)).collect();
            let mut seen = BTreeSet::new();
            let repeated = live.iter().any(|(j, _)| !seen.insert(*j));
            let out_of_range = live.iter().any(|(j, _)| usize::from(*j) >= joints_known);
            let not_finite = live.iter().any(|(_, w)| !w.is_finite());
            repeated || out_of_range || not_finite
        })
        .count()
}

/// How many of `mesh`'s vertices `brush` reaches: lies closer to its centre than its radius, with any strength.
fn in_reach(mesh: &PartMesh, brush: &WeightBrush) -> usize {
    mesh.geometry.positions.iter().filter(|p| reaches_point(**p, brush)).count()
}

/// Whether `brush` reaches the point `p`, by the test `apply_brushes` makes: inside the radius, with some strength.
fn reaches_point(p: Vec3, brush: &WeightBrush) -> bool {
    let centre = Vec3::new(narrow(brush.centre[0]), narrow(brush.centre[1]), narrow(brush.centre[2]));
    brush.strength > 0.0 && p.distance(centre) < narrow(brush.radius)
}

register_op!(WeightsReport);

#[cfg(test)]
mod tests {
    use omuzaki_char_api::{MaterialName, MeshData, PartMesh};

    use super::*;

    /// A one-vertex skinned mesh over `joint_bones`, with the given slots and weights.
    fn mesh(joint_bones: Vec<JointRef>, joints: [u16; 4], weights: [f32; 4]) -> PartMesh {
        PartMesh {
            name: "test".to_owned(),
            geometry: MeshData {
                positions: vec![Vec3::ZERO],
                joints: vec![joints],
                weights: vec![weights],
                ..MeshData::default()
            },
            joint_bones,
            material: "test-skin".parse::<MaterialName>().unwrap(),
            casts_shadow: false,
            render_order: 0,
            dynamic: false,
        }
    }

    #[test]
    fn a_malformed_skin_is_counted_and_a_clean_one_is_not() {
        let bones = vec![JointRef::HEAD, JointRef::CHEST];
        let half = [0.5, 0.5, 0.0, 0.0];
        assert_eq!(malformed_count(&mesh(bones.clone(), [0, 1, 0, 0], half)), 0, "a clean skin");
        assert_eq!(malformed_count(&mesh(bones.clone(), [0, 7, 0, 0], half)), 1, "a joint past the list");
        assert_eq!(malformed_count(&mesh(bones.clone(), [1, 1, 0, 0], half)), 1, "a joint named twice");
        assert_eq!(malformed_count(&mesh(bones.clone(), [0, 1, 0, 0], [f32::NAN, 0.5, 0.0, 0.0])), 1, "a NaN weight");
        assert_eq!(malformed_count(&mesh(bones, [0, 9, 0, 0], [1.0, 0.0, 0.0, 0.0])), 0, "an unused slot is ignored");
    }

    #[test]
    fn influences_count_the_generated_skin_plus_each_brush_joint_that_reaches() {
        let bones = vec![JointRef::HEAD, JointRef::CHEST];
        let base = mesh(bones, [0, 1, 0, 0], [0.5, 0.5, 0.0, 0.0]);
        let reaching = |joint: JointRef| WeightBrush {
            centre: [0.0; 3],
            radius: 0.1,
            joint,
            strength: 0.5,
            falloff: omuzaki_char_api::BrushFalloff::Constant,
        };
        let three = [reaching(JointRef::HIPS), reaching(JointRef::NECK), reaching(JointRef::HIPS)];
        assert_eq!(influences_over_limit(&base, &three), 0, "two skin joints and two new ones make four");
        let four =
            [reaching(JointRef::HIPS), reaching(JointRef::NECK), reaching(JointRef::Named("x".parse().unwrap()))];
        assert_eq!(influences_over_limit(&base, &four), 1, "five joints before the limit trims one");
    }
}
