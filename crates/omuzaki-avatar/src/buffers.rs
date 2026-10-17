//! The per-frame outputs of the evaluator, sized once per avatar so a frame never allocates.
//!
//! [`MorphWeights`] is laid out the way the renderer's scene wants it: one flat list of weights, mesh after mesh, each
//! mesh's targets in its morph set's order, so a mesh's range in it is what an instance's `morphs` points to.
//! [`BindDeltas`] holds, per material a bind moves, what the expressions add to its colours and UV transform, and
//! whether that moved enough since it was last marked to re-send the material.

use std::ops::Range;

use omuzaki_avatar_api::expression::ExprIx;
use omuzaki_char_api::MaterialName;
use omuzaki_core::glam::{Vec2, Vec4};
use omuzaki_io_api::ColorBindKind;

/// The smallest change of a material value that re-sends the material (`byom.md` D3): finer steps can't be seen in an
/// 8-bit colour and only cost uploads.
pub const MOVE_THRESHOLD: f32 = 1.0 / 1024.0;

/// How many colours a colour bind can move ([`ColorBindKind`]).
pub const COLOUR_KINDS: usize = 6;

/// The slot of a colour kind in [`MaterialDelta`]'s colour list.
pub(crate) const fn colour_slot(kind: ColorBindKind) -> usize {
    match kind {
        ColorBindKind::Color => 0,
        ColorBindKind::EmissionColor => 1,
        ColorBindKind::ShadeColor => 2,
        ColorBindKind::MatcapColor => 3,
        ColorBindKind::RimColor => 4,
        ColorBindKind::OutlineColor => 5,
    }
}

/// Morph weights for every mesh of one avatar, and the weight each expression was applied with.
#[derive(Clone, Debug, PartialEq)]
pub struct MorphWeights {
    /// Every mesh's target weights, mesh after mesh.
    pub(crate) weights: Vec<f32>,
    /// Where each mesh's weights start in `weights`, plus the end: one more entry than there are meshes.
    pub(crate) offsets: Vec<u32>,
    /// Each expression's weight after the inputs, `isBinary` and the overrides, in expression order.
    pub(crate) expressions: Vec<f32>,
}

impl MorphWeights {
    /// Zeroed buffers for meshes with these target counts and `expressions` expressions.
    pub(crate) fn new(targets_per_mesh: impl Iterator<Item = usize>, expressions: usize) -> Self {
        let mut offsets = vec![0];
        let mut total = 0usize;
        for targets in targets_per_mesh {
            total += targets;
            // The scene indexes morph weights with `u32`; an avatar with four billion targets is not one a file holds.
            debug_assert!(u32::try_from(total).is_ok(), "more morph targets than a u32 can index");
            offsets.push(u32::try_from(total).unwrap_or(u32::MAX));
        }
        Self { weights: vec![0.0; total], offsets, expressions: vec![0.0; expressions] }
    }

    /// Every weight, mesh after mesh: what the scene's morph weight list holds for this avatar.
    pub fn as_slice(&self) -> &[f32] {
        &self.weights
    }

    /// The range of mesh `mesh`'s weights in [`as_slice`](Self::as_slice) (empty for a mesh without targets), or
    /// `None` past the last mesh.
    pub fn range(&self, mesh: usize) -> Option<Range<u32>> {
        Some(*self.offsets.get(mesh)?..*self.offsets.get(mesh + 1)?)
    }

    /// The weights of mesh `mesh`'s targets, in its morph set's order (empty past the last mesh).
    pub fn mesh(&self, mesh: usize) -> &[f32] {
        self.range(mesh).and_then(|r| self.weights.get(r.start as usize..r.end as usize)).unwrap_or_default()
    }

    /// How many meshes it covers.
    pub fn meshes(&self) -> usize {
        self.offsets.len().saturating_sub(1)
    }

    /// The weight `expression` was applied with this frame (0 past the last expression).
    pub fn expression(&self, expression: ExprIx) -> f32 {
        self.expressions.get(expression.index()).copied().unwrap_or(0.0)
    }

    /// Every expression's applied weight, in expression order.
    pub fn expressions(&self) -> &[f32] {
        &self.expressions
    }
}

/// What the expressions add to one material this frame.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct MaterialDelta {
    pub(crate) colour: [Vec4; COLOUR_KINDS],
    pub(crate) uv_scale: Vec2,
    pub(crate) uv_offset: Vec2,
    pub(crate) moved: bool,
}

impl MaterialDelta {
    /// What is added to the colour of `kind`: the sum over its colour binds of `(target − base) · weight`, as linear
    /// RGBA. Only the base colour ([`ColorBindKind::Color`]) has an alpha; the others ignore the fourth component.
    pub fn colour(&self, kind: ColorBindKind) -> Vec4 {
        self.colour[colour_slot(kind)]
    }

    /// What is added to the material's UV scale.
    pub fn uv_scale(&self) -> Vec2 {
        self.uv_scale
    }

    /// What is added to the material's UV offset.
    pub fn uv_offset(&self) -> Vec2 {
        self.uv_offset
    }

    /// True if a value differs by more than [`MOVE_THRESHOLD`] from the values last marked moved, so the material is
    /// re-sent this frame. A slow drift accumulates until it crosses, so nothing is lost by skipping small steps.
    pub fn moved(&self) -> bool {
        self.moved
    }

    /// The largest difference of any value from `other`'s.
    fn distance(&self, other: &Self) -> f32 {
        let colours = self.colour.iter().zip(&other.colour).map(|(a, b)| (*a - *b).abs().max_element());
        let uv = [
            (self.uv_scale - other.uv_scale).abs().max_element(),
            (self.uv_offset - other.uv_offset).abs().max_element(),
        ];
        colours.chain(uv).fold(0.0, f32::max)
    }
}

/// Bind deltas for every material an expression of one avatar moves, in material name order.
#[derive(Clone, Debug, PartialEq)]
pub struct BindDeltas {
    /// The materials, sorted.
    pub(crate) materials: Vec<MaterialName>,
    /// This frame's deltas, one per material.
    pub(crate) deltas: Vec<MaterialDelta>,
    /// The deltas as they were last marked moved, one per material.
    pub(crate) sent: Vec<MaterialDelta>,
}

impl BindDeltas {
    /// Zeroed deltas for `materials` (sorted, unique).
    pub(crate) fn new(materials: Vec<MaterialName>) -> Self {
        let zero = vec![MaterialDelta::default(); materials.len()];
        Self { materials, deltas: zero.clone(), sent: zero }
    }

    /// Clears this frame's deltas before the binds accumulate.
    pub(crate) fn clear(&mut self) {
        for d in &mut self.deltas {
            *d = MaterialDelta::default();
        }
    }

    /// Marks each material that moved past the threshold since it was last marked, and remembers what was marked.
    pub(crate) fn mark_moved(&mut self) {
        for (delta, sent) in self.deltas.iter_mut().zip(&mut self.sent) {
            delta.moved = delta.distance(sent) > MOVE_THRESHOLD;
            if delta.moved {
                *sent = *delta;
            }
        }
    }

    /// Every material and its delta, in name order.
    pub fn iter(&self) -> impl Iterator<Item = (&MaterialName, &MaterialDelta)> {
        self.materials.iter().zip(&self.deltas)
    }

    /// The delta of `material`, if a bind moves it.
    pub fn get(&self, material: &MaterialName) -> Option<&MaterialDelta> {
        let index = self.materials.binary_search(material).ok()?;
        self.deltas.get(index)
    }

    /// How many materials binds move.
    pub fn len(&self) -> usize {
        self.materials.len()
    }

    /// True if no bind moves any material.
    pub fn is_empty(&self) -> bool {
        self.materials.is_empty()
    }
}

impl<'a> IntoIterator for &'a BindDeltas {
    type Item = (&'a MaterialName, &'a MaterialDelta);
    type IntoIter = std::iter::Zip<std::slice::Iter<'a, MaterialName>, std::slice::Iter<'a, MaterialDelta>>;

    fn into_iter(self) -> Self::IntoIter {
        self.materials.iter().zip(&self.deltas)
    }
}
