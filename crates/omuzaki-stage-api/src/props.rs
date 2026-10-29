//! Props (design D7, ST-03): [`PropDef`], a scene's furniture and things in hands, and the [`PropKind`] registry that
//! turns a prop's `params` into meshes, materials and practical lights.
//!
//! A kind is our generator (ADR 0019 D1): parameterised, procedural, licence-clean by construction. Kinds register
//! themselves next to their code with [`register_prop_kind!`] (ADR 0054), so neither the ops, nor the stage, nor the MCP
//! server keep a list. A prop naming a kind nothing registered is a [`Finding`](crate::Finding), not a refusal: the
//! stage skips it and the scene keeps it, so a project opened by a build without that kind loses nothing.

use std::fmt;

use omuzaki_anim_api::HumanoidBone;
use omuzaki_core::glam::{EulerRot, Mat4, Quat, Vec3};
use omuzaki_core::mesh::MeshData;
use omuzaki_core::slug_id;
use omuzaki_doc_api::Value;
use omuzaki_render_api::{Light, Material};
use schemars::JsonSchema;
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::anchor::Anchor;

slug_id!(
    /// A registered prop kind: `desk`, `shelf`, `lamp`.
    PropKindId
);

/// The most meshes one prop builds: a kind that needs more groups its pieces by material.
pub const MAX_PROP_PARTS: usize = 16;
/// The most practical lights one prop brings (a lamp's bulb, a window's daylight).
pub const MAX_PROP_LIGHTS: usize = 2;
/// The most triangles one prop builds: a shelf of books at its largest is about a third of this.
pub const MAX_PROP_TRIANGLES: usize = 60_000;

/// One prop of a scene (`scenes/<id>/props/<prop>.toml`, design D7).
///
/// ```toml
/// kind = "cup"
/// params = { colour = "#d9c7f0", height = 0.1 }
/// anchor = "stage"
/// position = [0.25, 0.74, 0.1]
/// ```
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PropDef {
    /// The registered kind that builds it.
    pub kind: PropKindId,
    /// The kind's parameters; what is left out takes the kind's default. Checked against the kind's schema by the
    /// ops; the stored value is what the kind's own check returned, so it holds every default.
    #[serde(default = "empty_object", skip_serializing_if = "is_empty_object")]
    pub params: Value,
    /// What [`position`](Self::position) is measured from: the stage origin, or a member's feet and facing (metres
    /// either way: a mug is 10 cm whoever holds it).
    #[serde(default = "stage_anchor", skip_serializing_if = "is_stage")]
    pub anchor: Anchor,
    /// A humanoid bone of the anchor member (`rightHand`, `head`): the prop follows it every frame (ST-03), and
    /// `position` and `rotation` are in the bone's frame. Needs a `subject` or `member:<id>` anchor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attach: Option<HumanoidBone>,
    /// Metres in the anchor's (or the bone's) frame.
    #[serde(default)]
    pub position: [f32; 3],
    /// Yaw, pitch and roll in degrees, applied in that order (about +Y, then +X, then +Z).
    #[serde(default)]
    pub rotation: [f32; 3],
    /// A uniform scale, `> 0`.
    #[serde(default = "one")]
    pub scale: f32,
    /// Hidden props stay in the scene and are not drawn.
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub visible: bool,
    /// Whether the prop shadows the cast and other props.
    #[serde(default = "yes", skip_serializing_if = "is_true")]
    pub casts_shadow: bool,
}

fn empty_object() -> Value {
    Value::Object(serde_json::Map::new())
}

fn is_empty_object(value: &Value) -> bool {
    value.as_object().is_some_and(serde_json::Map::is_empty)
}

fn stage_anchor() -> Anchor {
    Anchor::Stage
}

fn is_stage(anchor: &Anchor) -> bool {
    *anchor == Anchor::Stage
}

fn one() -> f32 {
    1.0
}

fn yes() -> bool {
    true
}

#[allow(clippy::trivially_copy_pass_by_ref)] // serde's `skip_serializing_if` passes a reference
fn is_true(value: &bool) -> bool {
    *value
}

impl PropDef {
    /// A visible prop of `kind` with its default parameters at the stage origin.
    #[must_use]
    pub fn new(kind: PropKindId) -> Self {
        Self {
            kind,
            params: empty_object(),
            anchor: Anchor::Stage,
            attach: None,
            position: [0.0; 3],
            rotation: [0.0; 3],
            scale: 1.0,
            visible: true,
            casts_shadow: true,
        }
    }

    /// The prop's own transform in its anchor's (or bone's) frame: translate, then yaw, pitch and roll, then scale.
    #[must_use]
    pub fn local(&self) -> Mat4 {
        local_transform(self.position, self.rotation, self.scale)
    }
}

/// Translate to `position`, turn by `rotation` (yaw, pitch, roll in degrees: about +Y, then +X, then +Z), scale by
/// `scale`: the one placement rule for props and the pieces a kind lays out.
#[must_use]
pub fn local_transform(position: [f32; 3], rotation: [f32; 3], scale: f32) -> Mat4 {
    let [yaw, pitch, roll] = rotation.map(f32::to_radians);
    Mat4::from_scale_rotation_translation(
        Vec3::splat(scale),
        Quat::from_euler(EulerRot::YXZ, yaw, pitch, roll),
        Vec3::from_array(position),
    )
}

/// One mesh of a built prop, in the prop's own frame (metres, +Y up, its front toward +Z).
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct PropPart {
    /// The geometry. Rigid: a prop never skins.
    pub mesh: MeshData,
    /// How it is drawn.
    pub material: Material,
    /// Whether this piece casts a shadow when the prop does (a window's glass doesn't).
    pub casts_shadow: bool,
}

impl PropPart {
    /// A piece that casts shadows.
    #[must_use]
    pub fn new(mesh: MeshData, material: Material) -> Self {
        Self { mesh, material, casts_shadow: true }
    }

    /// The same piece, casting no shadow.
    #[must_use]
    pub fn without_shadow(mut self) -> Self {
        self.casts_shadow = false;
        self
    }
}

/// What a kind builds from its parameters.
#[derive(Clone, Debug, Default, PartialEq)]
#[non_exhaustive]
pub struct PropOutput {
    /// The meshes, at most [`MAX_PROP_PARTS`].
    pub parts: Vec<PropPart>,
    /// Practical lights in the prop's frame (positions in metres, directions as given), at most [`MAX_PROP_LIGHTS`].
    /// They join the scene's lights after its own, while room is left under the renderer's limit.
    pub lights: Vec<Light>,
}

impl PropOutput {
    /// The number of triangles over every part.
    #[must_use]
    pub fn triangles(&self) -> usize {
        self.parts.iter().map(|p| p.mesh.triangle_count()).sum()
    }

    /// Checks the limits and every mesh: what the stage relies on before it uploads anything.
    ///
    /// # Errors
    /// [`PropError::Build`] naming the first broken rule.
    pub fn check(&self, kind: &str) -> Result<(), PropError> {
        let bad = |reason: String| Err(PropError::Build { kind: kind.to_owned(), reason });
        if self.parts.is_empty() {
            return bad("it built no mesh".to_owned());
        }
        if self.parts.len() > MAX_PROP_PARTS {
            return bad(format!("{} meshes, more than the {MAX_PROP_PARTS} a prop may have", self.parts.len()));
        }
        if self.lights.len() > MAX_PROP_LIGHTS {
            return bad(format!("{} lights, more than the {MAX_PROP_LIGHTS} a prop may have", self.lights.len()));
        }
        let triangles = self.triangles();
        if triangles > MAX_PROP_TRIANGLES {
            return bad(format!("{triangles} triangles, more than the {MAX_PROP_TRIANGLES} a prop may have"));
        }
        for (i, part) in self.parts.iter().enumerate() {
            if part.mesh.is_skinned() {
                return bad(format!("mesh {i} is skinned, and a prop is rigid"));
            }
            if let Err(e) = part.mesh.validate() {
                return bad(format!("mesh {i}: {}", e.0));
            }
            if let Err(e) = part.material.validate() {
                return bad(format!("material {i}: {e}"));
            }
        }
        Ok(())
    }
}

/// Why a prop's parameters or its build were refused.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum PropError {
    /// No kind of that name is registered in this build.
    #[error("no prop kind `{kind}` (known: {known})")]
    UnknownKind {
        /// The kind asked for.
        kind: String,
        /// The registered kinds, comma-separated.
        known: String,
    },
    /// The parameters don't decode or break a rule of the kind.
    #[error("the {kind} parameters: {reason}")]
    Params {
        /// The kind.
        kind: String,
        /// What is wrong, naming the field.
        reason: String,
    },
    /// The kind could not build from parameters it accepted, or built something the stage can't draw.
    #[error("building the {kind}: {reason}")]
    Build {
        /// The kind.
        kind: String,
        /// What went wrong.
        reason: String,
    },
}

/// A prop kind: a generator from typed parameters to meshes, materials and lights.
///
/// Implement it on a unit struct next to the kind's code and register it with [`register_prop_kind!`].
pub trait PropKind: 'static {
    /// The parameters, with serde defaults for every field so `{}` is the kind at its default.
    type Params: Default + Serialize + DeserializeOwned + JsonSchema;

    /// The kind's id, a slug.
    const ID: &'static str;
    /// One line for people and agents: what it is and what its parameters change.
    const SUMMARY: &'static str;

    /// The rules serde can't express (ranges, counts). The default accepts what decodes.
    ///
    /// # Errors
    /// The first broken rule, naming the field.
    fn validate(params: &Self::Params) -> Result<(), String> {
        let _ = params;
        Ok(())
    }

    /// Builds the prop in its own frame: metres, +Y up, standing on `y = 0`, its front toward +Z.
    ///
    /// # Errors
    /// A reason, for parameters [`validate`](Self::validate) let through and the kind still can't build.
    fn build(params: &Self::Params) -> Result<PropOutput, String>;
}

/// One registered kind, type-erased. Made by [`register_prop_kind!`].
pub struct PropKindRegistration {
    /// The kind's id.
    pub id: &'static str,
    /// Its one-line summary.
    pub summary: &'static str,
    schema: fn() -> Value,
    defaults: fn() -> Value,
    check: fn(&Value) -> Result<Value, PropError>,
    build: fn(&Value) -> Result<PropOutput, PropError>,
}

impl fmt::Debug for PropKindRegistration {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PropKindRegistration").field("id", &self.id).finish_non_exhaustive()
    }
}

impl PropKindRegistration {
    /// The registration of `K`.
    #[must_use]
    pub const fn of<K: PropKind>() -> Self {
        Self {
            id: K::ID,
            summary: K::SUMMARY,
            schema: schema_of::<K>,
            defaults: defaults_of::<K>,
            check: check_as::<K>,
            build: build_as::<K>,
        }
    }

    /// The JSON schema of the kind's parameters (draft 2020-12, subschemas inlined).
    #[must_use]
    pub fn schema(&self) -> Value {
        (self.schema)()
    }

    /// The kind's parameters at their defaults.
    #[must_use]
    pub fn defaults(&self) -> Value {
        (self.defaults)()
    }

    /// Decodes and validates `params`, and returns them with every default filled in: the value an op stores.
    ///
    /// # Errors
    /// [`PropError::Params`] naming the field.
    pub fn check(&self, params: &Value) -> Result<Value, PropError> {
        (self.check)(params)
    }

    /// Builds the prop from `params` and checks what came out ([`PropOutput::check`]).
    ///
    /// # Errors
    /// [`PropError::Params`] for parameters that don't check, [`PropError::Build`] for a build that fails or comes
    /// out undrawable.
    pub fn build(&self, params: &Value) -> Result<PropOutput, PropError> {
        let output = (self.build)(params)?;
        output.check(self.id)?;
        Ok(output)
    }
}

fn schema_of<K: PropKind>() -> Value {
    let schema = schemars::generate::SchemaSettings::draft2020_12()
        .with(|s| s.inline_subschemas = true)
        .into_generator()
        .into_root_schema_for::<K::Params>();
    serde_json::to_value(schema).unwrap_or(Value::Null)
}

fn defaults_of<K: PropKind>() -> Value {
    serde_json::to_value(K::Params::default()).unwrap_or(Value::Null)
}

fn decode<K: PropKind>(params: &Value) -> Result<K::Params, PropError> {
    let bad = |reason: String| PropError::Params { kind: K::ID.to_owned(), reason };
    let params = if params.is_null() { empty_object() } else { params.clone() };
    let decoded: K::Params = serde_json::from_value(params).map_err(|e| bad(e.to_string()))?;
    K::validate(&decoded).map_err(bad)?;
    Ok(decoded)
}

fn check_as<K: PropKind>(params: &Value) -> Result<Value, PropError> {
    let decoded = decode::<K>(params)?;
    serde_json::to_value(decoded).map_err(|e| PropError::Params { kind: K::ID.to_owned(), reason: e.to_string() })
}

fn build_as<K: PropKind>(params: &Value) -> Result<PropOutput, PropError> {
    let decoded = decode::<K>(params)?;
    K::build(&decoded).map_err(|reason| PropError::Build { kind: K::ID.to_owned(), reason })
}

inventory::collect!(PropKindRegistration);

/// Registers a prop kind so the stage draws it and the ops, the MCP server and the UI offer it. Put it next to the
/// kind.
///
/// ```ignore
/// omuzaki_stage_api::register_prop_kind!(Desk);
/// ```
#[macro_export]
macro_rules! register_prop_kind {
    ($ty:ty) => {
        $crate::__inventory::submit! { $crate::props::PropKindRegistration::of::<$ty>() }
        $crate::__record_registration!("prop_kind", $ty);
    };
}

/// Every registered prop kind, sorted by id.
#[must_use]
pub fn prop_kinds() -> Vec<&'static PropKindRegistration> {
    let mut kinds: Vec<_> = inventory::iter::<PropKindRegistration>.into_iter().collect();
    kinds.sort_by_key(|k| k.id);
    kinds
}

/// The registration of the kind called `id`.
#[must_use]
pub fn find_prop_kind(id: &str) -> Option<&'static PropKindRegistration> {
    inventory::iter::<PropKindRegistration>.into_iter().find(|k| k.id == id)
}

/// The registration of `kind`, or [`PropError::UnknownKind`] listing the known ones.
///
/// # Errors
/// [`PropError::UnknownKind`].
pub fn prop_kind(kind: &str) -> Result<&'static PropKindRegistration, PropError> {
    find_prop_kind(kind).ok_or_else(|| PropError::UnknownKind {
        kind: kind.to_owned(),
        known: prop_kinds().iter().map(|k| k.id).collect::<Vec<_>>().join(", "),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use omuzaki_render_api::Colour;

    #[derive(Default, Serialize, Deserialize, JsonSchema)]
    #[serde(default, deny_unknown_fields)]
    struct BlockParams {
        size: f32,
    }

    struct Block;

    impl PropKind for Block {
        type Params = BlockParams;
        const ID: &'static str = "test-block";
        const SUMMARY: &'static str = "a block, for the registry's tests";

        fn validate(p: &BlockParams) -> Result<(), String> {
            if p.size >= 0.0 { Ok(()) } else { Err(format!("size must be 0 or more, got {}", p.size)) }
        }

        fn build(p: &BlockParams) -> Result<PropOutput, String> {
            let h = p.size.max(0.01) * 0.5;
            let mut mesh = MeshData::default();
            for (x, y) in [(-h, 0.0), (h, 0.0), (0.0, 2.0 * h)] {
                mesh.positions.push(Vec3::new(x, y, 0.0));
                mesh.normals.push(Vec3::Z);
            }
            mesh.indices = vec![0, 1, 2];
            Ok(PropOutput { parts: vec![PropPart::new(mesh, Material::toon(Colour::WHITE))], lights: Vec::new() })
        }
    }

    crate::register_prop_kind!(Block);

    #[test]
    fn a_kind_registered_here_is_found_and_checks_its_params() {
        let kind = prop_kind("test-block").unwrap();
        assert_eq!(kind.check(&serde_json::json!({})).unwrap(), serde_json::json!({"size": 0.0}));
        let err = kind.check(&serde_json::json!({"size": -1.0})).unwrap_err();
        assert!(err.to_string().contains("size must be 0 or more"), "{err}");
        let err = kind.check(&serde_json::json!({"sise": 1.0})).unwrap_err();
        assert!(err.to_string().contains("unknown field `sise`"), "{err}");
        assert_eq!(kind.build(&serde_json::json!({"size": 1.0})).unwrap().triangles(), 1);
        let err = prop_kind("no-such").unwrap_err();
        assert!(err.to_string().contains("test-block"), "the known kinds are listed: {err}");
    }

    #[test]
    fn a_prop_writes_only_what_differs_from_its_defaults() {
        let prop = PropDef::new(PropKindId::new("cup").unwrap());
        assert_eq!(serde_json::to_value(&prop).unwrap(), serde_json::json!({"kind": "cup", "position": [0.0, 0.0, 0.0], "rotation": [0.0, 0.0, 0.0], "scale": 1.0}));
        let back: PropDef = serde_json::from_value(serde_json::json!({"kind": "cup"})).unwrap();
        assert_eq!(back, prop);
    }

    #[test]
    fn the_local_transform_turns_yaw_first() {
        let m = local_transform([1.0, 2.0, 3.0], [90.0, 0.0, 0.0], 2.0);
        let p = m.transform_point3(Vec3::X);
        assert!(p.abs_diff_eq(Vec3::new(1.0, 2.0, 1.0), 1e-5), "{p}");
    }
}
