//! The draw list: what a renderer needs to draw one evaluated frame of a puppet, in order, back to front.
//!
//! Positions are in model space; the renderer applies its view (camera) on top. Colours are premultiplied sRGB, as
//! the textures are. A command list is reused frame to frame ([`DrawList2D::clear`] keeps the capacity), so building
//! one allocates nothing once warm.

use crate::model::{BlendMode, MaskMode, TexIx};

/// A vertex: where, and where in its texture.
#[derive(Copy, Clone, Debug, Default, PartialEq)]
pub struct Vertex2D {
    /// Position, model units.
    pub pos: [f32; 2],
    /// Texture coordinate, `0..1`, y down.
    pub uv: [f32; 2],
}

/// How a mesh or a composite's layer is coloured and blended.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct DrawState {
    /// The blend mode.
    pub blend: BlendMode,
    /// Opacity, `0..1`, after every binding.
    pub opacity: f32,
    /// Multiplied tint.
    pub tint: [f32; 3],
    /// Screened tint.
    pub screen_tint: [f32; 3],
    /// Whether the current mask applies.
    pub masked: bool,
}

impl Default for DrawState {
    fn default() -> Self {
        Self { blend: BlendMode::Normal, opacity: 1.0, tint: [1.0; 3], screen_tint: [0.0; 3], masked: false }
    }
}

/// One command.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum DrawCmd2D {
    /// Clears the mask; the [`DrawCmd2D::MaskShape`]s that follow define it.
    MaskBegin,
    /// Adds a shape to the mask: where the texture's alpha passes `threshold` (a mask mesh with no texture covers all
    /// of its triangles). A pixel is inside the mask where any `Mask` shape covers it and no `Dodge` shape does; with
    /// only `Dodge` shapes, everywhere they don't cover.
    MaskShape {
        /// The texture whose alpha shapes it, if any.
        texture: Option<TexIx>,
        /// The first index of its triangles in [`DrawList2D::indices`].
        first: u32,
        /// How many indices.
        count: u32,
        /// Inside or outside.
        mode: MaskMode,
        /// The alpha that counts as covering.
        threshold: f32,
    },
    /// Draws textured triangles.
    Mesh {
        /// The texture.
        texture: TexIx,
        /// The first index.
        first: u32,
        /// How many indices.
        count: u32,
        /// How it draws.
        state: DrawState,
    },
    /// Starts a composite: what follows draws into a fresh transparent layer, until the matching end.
    CompositeBegin,
    /// Ends the innermost composite and blends its layer onto the one under it.
    CompositeEnd {
        /// How the layer draws.
        state: DrawState,
    },
}

/// A frame's draw list.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct DrawList2D {
    /// Every vertex of the frame.
    pub vertices: Vec<Vertex2D>,
    /// Triangles, three indices each, into `vertices`.
    pub indices: Vec<u32>,
    /// The commands, back to front.
    pub cmds: Vec<DrawCmd2D>,
}

impl DrawList2D {
    /// Empties the list, keeping its memory.
    pub fn clear(&mut self) {
        self.vertices.clear();
        self.indices.clear();
        self.cmds.clear();
    }

    /// The bounding box of every vertex (`[min, max]`), or `None` for an empty list.
    #[must_use]
    pub fn bounds(&self) -> Option<[[f32; 2]; 2]> {
        let mut it = self.vertices.iter();
        let first = it.next()?.pos;
        Some(it.fold([first, first], |[lo, hi], v| {
            [[lo[0].min(v.pos[0]), lo[1].min(v.pos[1])], [hi[0].max(v.pos[0]), hi[1].max(v.pos[1])]]
        }))
    }
}
