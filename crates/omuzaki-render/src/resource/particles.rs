//! Particle systems (particles.md D2, D6): a system's description packed once into the header every block of its
//! batches starts with, and the material its sprites are shaded through.
//!
//! [`SystemUniform`] must match `struct ParticleSystem` in `shader/particles_state.wgsl` field for field. The per-frame
//! fields (the density and the number of batches in a block) are left zero here and filled in by the `Particles` pass
//! as it packs each block.

use bytemuck::{Pod, Zeroable};
use omuzaki_render_api::{
    Colour, Material, Orientation, ParticleShading, ParticleSystemDesc, Rim, Shading, Sprite, SpriteShape, Toon,
};

use crate::arena::Arena;
use crate::resource::material::{self, GpuMaterial};
use crate::resource::texture::GpuTexture;
use crate::shared::Shared;

/// A system's parameters as the shader reads them: 256 bytes, the head of every block of its batches.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub(crate) struct SystemUniform {
    /// Lifetime min and max (seconds), speed min and max (metres per second).
    lifetime_speed: [f32; 4],
    /// The emission axis (emitter space) and the cosine of the cone's half-angle.
    direction: [f32; 4],
    /// Gravity (metres per second squared) and drag (per second).
    gravity: [f32; 4],
    /// Wind (metres per second) and glow.
    wind: [f32; 4],
    /// Flutter amplitude (metres) and frequency (hertz), 1 when there is flutter, the floor height.
    flutter: [f32; 4],
    /// Size min and max (metres), coverage, the quad's aspect.
    size: [f32; 4],
    /// The size multiplier at life 0, 1/3, 2/3 and 1.
    size_curve: [f32; 4],
    /// Spin or tumble rate min and max (degrees per second), the velocity stretch, the edge's shade.
    turn: [f32; 4],
    /// The edge's width in pixels (0: no edge), 1 when there is a floor, the frame's density, unused.
    pub(crate) edge: [f32; 4],
    /// The sprite shape's code, the orientation's code, a star's points, 1 for an atlas.
    codes: [u32; 4],
    /// Atlas columns, rows and cells, and the number of batches in the block.
    pub(crate) atlas: [u32; 4],
    padding: [[f32; 4]; 5],
}

const _: () = assert!(size_of::<SystemUniform>() == 256, "a system's header is one 256-byte slot");

impl SystemUniform {
    pub(crate) fn new(desc: &ParticleSystemDesc) -> Self {
        let p = &desc.params;
        let (turn_span, stretch) = match p.orientation {
            Orientation::Billboard { spin_deg, .. } => (spin_deg, 0.0),
            Orientation::Velocity { stretch, .. } => (omuzaki_render_api::Span::fixed(0.0), stretch),
            Orientation::Tumble { rate_deg, .. } => (rate_deg, 0.0),
        };
        let (shape, points, atlas, columns, rows) = match desc.sprite {
            Sprite::Shape(shape) => {
                let points = if let SpriteShape::Star { points, .. } = shape { u32::from(points) } else { 0 };
                (shape.code(), points, 0, 1, 1)
            }
            Sprite::Atlas { columns, rows, .. } => (0, 0, 1, u32::from(columns), u32::from(rows)),
        };
        let flutter = p.flutter.map_or([0.0; 3], |f| [f.amplitude_m, f.frequency_hz, 1.0]);
        let edge = p.edge.map_or([0.0, 0.0], |e| [e.width_px, e.shade]);
        Self {
            lifetime_speed: [p.lifetime.min, p.lifetime.max, p.speed.min, p.speed.max],
            direction: [p.direction[0], p.direction[1], p.direction[2], p.spread_deg.to_radians().cos()],
            gravity: [p.gravity[0], p.gravity[1], p.gravity[2], p.drag],
            wind: [p.wind[0], p.wind[1], p.wind[2], p.glow],
            flutter: [flutter[0], flutter[1], flutter[2], p.floor.unwrap_or(0.0)],
            size: [p.size.min, p.size.max, p.coverage, desc.aspect()],
            size_curve: p.size_curve,
            turn: [turn_span.min, turn_span.max, stretch, edge[1]],
            edge: [edge[0], f32::from(u8::from(p.floor.is_some())), 1.0, 0.0],
            codes: [shape, p.orientation.code(), points, atlas],
            atlas: [columns, rows, columns * rows, 0],
            padding: [[0.0; 4]; 5],
        }
    }
}

/// The material a system's sprites are shaded through (D5): white, so the particle's own colour is the albedo; toon
/// with the system's shade, shift and softness and no rim, skin or shadows, or unlit; an atlas as the base texture.
pub(crate) fn shading_material(desc: &ParticleSystemDesc) -> Material {
    let mut material = match desc.params.shading {
        ParticleShading::Toon { shade, shift, softness, .. } => {
            let toon = Toon { shade, shift, softness, rim: Rim { strength: 0.0, ..Rim::default() }, ..Toon::default() };
            Material { shading: Shading::Toon(toon), receives_shadows: false, ..Material::toon(Colour::WHITE) }
        }
        ParticleShading::Unlit => Material::unlit(Colour::WHITE),
    };
    material.base_texture = desc.sprite.texture();
    material
}

/// A particle system on the GPU.
pub(crate) struct GpuParticleSystem {
    pub(crate) desc: ParticleSystemDesc,
    pub(crate) header: SystemUniform,
    /// Group 1 of the particle shader: the system's shading and its atlas.
    pub(crate) material: GpuMaterial,
}

impl GpuParticleSystem {
    /// Packs `desc`, whose atlas texture (if any) the caller has checked is live.
    pub(crate) fn create(shared: &Shared, textures: &Arena<GpuTexture>, desc: &ParticleSystemDesc) -> Self {
        Self {
            desc: desc.clone(),
            header: SystemUniform::new(desc),
            material: GpuMaterial::create(shared, textures, &shading_material(desc)),
        }
    }

    /// Replaces the description in place.
    pub(crate) fn update(&mut self, shared: &Shared, textures: &Arena<GpuTexture>, desc: &ParticleSystemDesc) {
        self.material.update(shared, textures, &shading_material(desc));
        self.header = SystemUniform::new(desc);
        self.desc = desc.clone();
    }

    /// The estimate of its GPU memory: the material's uniform (the header travels with the frame's blocks).
    pub(crate) const BYTES: u64 = material::UNIFORM_BYTES;
}
