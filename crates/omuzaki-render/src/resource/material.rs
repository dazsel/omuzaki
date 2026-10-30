//! Materials: the whole [`Material`] of the contract, packed into one uniform buffer and one bind group.
//!
//! The uniform is complete on purpose: it carries every field of the contract (toon, rim, skin, outline, overlays,
//! alpha mode, texture flags), not just what the passes of the day read. Toon shading, outlines, shadows and programs
//! then each read what they need from `shader/common.wgsl` without touching this file, which is what lets those tasks
//! proceed in parallel. [`MaterialUniform`] must match `struct Material` there field for field.

use bytemuck::{Pod, Zeroable};
use omuzaki_core::id::Slug;
use omuzaki_render_api::{
    AlphaMode, Colour, Mapping, Material, Mtoon, OutlineWidthMode, SURFACE_FX_SIZES, Shading, SurfaceFx, SurfaceFxKind,
    TextureId, Toon,
};

use crate::arena::Arena;
use crate::program::{PACKED_FLOATS, Registry, Resolved};
use crate::resource::texture::GpuTexture;
use crate::shared::{MATERIAL_TEXTURES, MTOON_BINDING, MTOON_TEXTURES, OUTLINE_WIDTH_BINDING, Shared};

/// Bits of `Material.flags.x` (`FLAG_*` in `shader/common.wgsl`).
pub(crate) mod flag {
    pub(crate) const DOUBLE_SIDED: u32 = 1;
    pub(crate) const VERTEX_COLOURS: u32 = 2;
    pub(crate) const RECEIVES_SHADOWS: u32 = 4;
    pub(crate) const SKIN: u32 = 8;
    pub(crate) const BASE_TEXTURE: u32 = 16;
    pub(crate) const OUTLINE: u32 = 32;
    /// The outline's width is metres at any distance (`OutlineWidthMode::World`).
    pub(crate) const OUTLINE_WORLD: u32 = 64;
    /// The outline's width is a fraction of the target's height (`OutlineWidthMode::Screen`).
    pub(crate) const OUTLINE_SCREEN: u32 = 128;
    /// The outline's width is multiplied by the green channel of the outline width texture.
    pub(crate) const OUTLINE_WIDTH_TEXTURE: u32 = 256;
    /// The hair shadow darkens the surface (`HairShadow::receives`).
    pub(crate) const RECEIVES_HAIR_SHADOW: u32 = 512;
}

/// Bits of `Material.mtoon_flags.x` (`MTOON_*` in the shader): the material is lit by `MToon`, and which of its
/// textures it has, in the slot order of [`mtoon_slots`].
pub(crate) mod mtoon {
    pub(crate) const ON: u32 = 1;
    pub(crate) const SHADE_TEXTURE: u32 = 2;
    pub(crate) const SHIFT_TEXTURE: u32 = 4;
    pub(crate) const MATCAP_TEXTURE: u32 = 8;
    pub(crate) const RIM_TEXTURE: u32 = 16;
    pub(crate) const EMISSIVE_TEXTURE: u32 = 32;
    pub(crate) const NORMAL_TEXTURE: u32 = 64;
    pub(crate) const MASK_TEXTURE: u32 = 128;
}

/// Values of `Material.flags.y` (`SHADING_*` in the shader).
pub(crate) mod shading {
    pub(crate) const TOON: u32 = 0;
    pub(crate) const UNLIT: u32 = 1;
}

/// Values of `Material.flags.z` (`ALPHA_*` in the shader).
pub(crate) mod alpha {
    pub(crate) const OPAQUE: u32 = 0;
    pub(crate) const MASK: u32 = 1;
    pub(crate) const BLEND: u32 = 2;
}

/// One overlay as the shader reads it.
#[repr(C)]
#[derive(Clone, Copy, Default, Pod, Zeroable)]
struct OverlayUniform {
    /// Planar mapping: lower-left x, y, upper-right x, y in the mesh's local space.
    rect: [f32; 4],
    /// Strength, fade start, fade end, and 1 for a planar mapping (0 for UV).
    params: [f32; 4],
}

/// Mirrors `struct Material` in `shader/common.wgsl`. All colours are linear light.
#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
pub(crate) struct MaterialUniform {
    /// Base colour rgb and alpha.
    base: [f32; 4],
    /// Toon shade multiplier rgb.
    shade: [f32; 4],
    /// Rim colour rgb and strength.
    rim: [f32; 4],
    /// Emissive rgb and skin hue-keep.
    emissive: [f32; 4],
    /// Outline colour rgb and width in metres (0 when the material has no outline).
    outline: [f32; 4],
    /// Toon shift, softness, rim power and skin floor.
    toon: [f32; 4],
    /// Flag bits, shading kind, alpha mode, overlay count.
    flags: [u32; 4],
    /// Mask cutoff.
    alpha: [f32; 4],
    /// Per-slot texture flags, four bits per slot (base, overlay 0..3, the outline width texture).
    textures: [u32; 4],
    overlays: [OverlayUniform; 4],
    /// Shading program parameters, packed in the program's declared order (`program/mod.rs`).
    program: [[f32; 4]; PACKED_FLOATS / 4],
    /// The UV transform: scale in xy, offset in zw (`uv * scale + offset`, as `UvTransform::apply`).
    uv_transform: [f32; 4],
    /// The outline's lighting mix in x (`OutlineStyle::lighting_mix`); the rest is zero.
    outline_mix: [f32; 4],
    /// `MToon`: the `mtoon::*` bits in x, four bits of texture flags per `MToon` slot in y.
    mtoon_flags: [u32; 4],
    /// `MToon`'s shade colour rgb and the shading shift texture's scale.
    mtoon_shade: [f32; 4],
    /// `MToon`'s shading shift, toony factor, GI equalisation and rim lighting mix.
    mtoon_shading: [f32; 4],
    /// `MToon`'s rim colour rgb and fresnel power.
    mtoon_rim: [f32; 4],
    /// `MToon`'s matcap colour rgb and the rim lift.
    mtoon_matcap: [f32; 4],
    /// `MToon`'s UV scroll per second (xy), rotation per second (z) and the normal map's scale (w).
    mtoon_uv: [f32; 4],
    /// The surface fx: its kind in x (`fx_kind::*`, 0 for none), its seed in y.
    fx_flags: [u32; 4],
    /// The surface fx's amount, one over its blob size, edge width and glow.
    fx: [f32; 4],
    /// The colour behind the front (rgb) and the sweep mix.
    fx_colour: [f32; 4],
    /// The edge band's colour (rgb) and where the sweep starts.
    fx_edge: [f32; 4],
    /// The sweep's unit direction (xyz, zero for none) and where it ends.
    fx_sweep: [f32; 4],
}

/// `SurfaceFxKind` as the shader reads it (`FX_*` in `common.wgsl`); 0 is no effect.
mod fx_kind {
    pub(super) const TINT: u32 = 1;
    pub(super) const DISSOLVE: u32 = 2;
}

/// The surface fx part of the uniform, all zeros (off) for `None`.
fn fx_uniform(fx: Option<&SurfaceFx>, uniform: &mut MaterialUniform) {
    let Some(fx) = fx else { return };
    let kind = match fx.kind {
        SurfaceFxKind::Tint => fx_kind::TINT,
        SurfaceFxKind::Dissolve => fx_kind::DISSOLVE,
    };
    let sweep = if fx.sweep_mix > 0.0 { fx.sweep.normalize_or_zero() } else { omuzaki_core::glam::Vec3::ZERO };
    uniform.fx_flags = [kind, fx.seed, 0, 0];
    uniform.fx = [fx.amount, 1.0 / fx.size.max(SURFACE_FX_SIZES.0), fx.edge_width, fx.glow];
    uniform.fx_colour = rgb(fx.colour, fx.sweep_mix);
    uniform.fx_edge = rgb(fx.edge, fx.sweep_range.x);
    uniform.fx_sweep = [sweep.x, sweep.y, sweep.z, fx.sweep_range.y];
}

/// Bytes of one material uniform, for the memory estimate.
pub(crate) const UNIFORM_BYTES: u64 = size_of::<MaterialUniform>() as u64;

/// The `MToon` parameters a material is lit by: its own when it has them and toon shading (a program or unlit wins).
pub(crate) fn mtoon_of(material: &Material) -> Option<&Mtoon> {
    material.mtoon.as_deref().filter(|_| matches!(material.shading, Shading::Toon(_)))
}

/// The `MToon` texture slots, in binding order (`shared::MTOON_TEXTURES`).
pub(crate) fn mtoon_slots(m: &Mtoon) -> [Option<TextureId>; MTOON_TEXTURES as usize] {
    [
        m.shade_texture,
        m.shading_shift_texture,
        m.matcap_texture,
        m.rim_texture,
        m.emissive_texture,
        m.normal_texture,
        m.uv_animation_mask,
    ]
}

/// A material's shading program, if it names a registered one. Parameters were checked when the material was created
/// or updated (`WgpuRenderer::check_material`), so a failure here cannot happen; it would only mean the default toon.
pub(crate) fn program_of(shading: &Shading) -> Option<Resolved> {
    Registry::global().ok()?.resolve(shading).ok().flatten()
}

/// The toon parameters and shading kind a material draws with, and its program's packed parameters (zeros without one).
///
/// A program the renderer does not know draws as the default toon over the same base, textures and overlays (contract
/// rule 5). A known one draws as toon with the base the program asks for, plus its shader hooks.
fn shading_of(shading: &Shading) -> (u32, Toon, [[f32; 4]; PACKED_FLOATS / 4]) {
    match shading {
        Shading::Toon(toon) => (self::shading::TOON, *toon, [[0.0; 4]; PACKED_FLOATS / 4]),
        Shading::Unlit => (self::shading::UNLIT, Toon::default(), [[0.0; 4]; PACKED_FLOATS / 4]),
        Shading::Program { .. } => match program_of(shading) {
            Some(resolved) => (self::shading::TOON, resolved.toon, resolved.packed),
            None => (self::shading::TOON, Toon::default(), [[0.0; 4]; PACKED_FLOATS / 4]),
        },
    }
}

/// The shading programs this renderer knows, for `Capabilities::shading_programs`: every program linked into the
/// binary (`program/mod.rs`), so the renderer's own code needs no edit to list a new one.
pub(crate) fn known_programs() -> Vec<Slug> {
    Registry::global().map(Registry::names).unwrap_or_default()
}

fn rgb(colour: Colour, w: f32) -> [f32; 4] {
    let c = colour.to_linear();
    [c.x, c.y, c.z, w]
}

impl MaterialUniform {
    pub(crate) fn new(material: &Material, textures: &Arena<GpuTexture>) -> Self {
        let (kind, toon, program) = shading_of(&material.shading);
        let texture_flags = |id: Option<TextureId>| {
            id.and_then(|id| textures.get(id.index(), id.generation())).map_or(0, |t| t.shader_flags)
        };
        // The outline width texture's flags sit after the five surface slots' (slot 5).
        let mut texture_bits =
            texture_flags(material.base_texture) | texture_flags(material.outline_style.width_texture) << 20;
        let mut overlays = [OverlayUniform::default(); 4];
        for (i, (slot, overlay)) in overlays.iter_mut().zip(&material.overlays).enumerate() {
            texture_bits |= texture_flags(Some(overlay.texture)) << (4 * (i + 1));
            *slot = match overlay.mapping {
                Mapping::Uv => OverlayUniform { rect: [0.0; 4], params: [overlay.strength, 0.0, 0.0, 0.0] },
                Mapping::Planar { min, max, fade_from, fade_to } => OverlayUniform {
                    rect: [min.x, min.y, max.x, max.y],
                    params: [overlay.strength, fade_from, fade_to, 1.0],
                },
            };
        }
        let (mask_cutoff, alpha_mode) = match material.alpha {
            AlphaMode::Opaque => (0.0, alpha::OPAQUE),
            AlphaMode::Mask { cutoff } => (cutoff, alpha::MASK),
            AlphaMode::Blend => (0.0, alpha::BLEND),
        };
        let mut bits = 0;
        for (on, bit) in [
            (material.double_sided, flag::DOUBLE_SIDED),
            (material.vertex_colours, flag::VERTEX_COLOURS),
            (material.receives_shadows, flag::RECEIVES_SHADOWS),
            (material.hair_shadow.receives, flag::RECEIVES_HAIR_SHADOW),
            (toon.skin.is_some() && kind == shading::TOON, flag::SKIN),
            (material.base_texture.is_some(), flag::BASE_TEXTURE),
            (material.outline.is_some(), flag::OUTLINE),
            (material.outline_style.width_mode == OutlineWidthMode::World, flag::OUTLINE_WORLD),
            (material.outline_style.width_mode == OutlineWidthMode::Screen, flag::OUTLINE_SCREEN),
            (material.outline_style.width_texture.is_some(), flag::OUTLINE_WIDTH_TEXTURE),
        ] {
            if on {
                bits |= bit;
            }
        }
        let skin = toon.skin.unwrap_or_default();
        let outline = material.outline.map_or([0.0; 4], |o| rgb(o.colour, o.width));
        let mut uniform = Self {
            base: rgb(material.base, material.base.alpha()),
            shade: rgb(toon.shade, 0.0),
            rim: rgb(toon.rim.colour, toon.rim.strength),
            emissive: rgb(toon.emissive, skin.hue_keep),
            outline,
            toon: [toon.shift, toon.softness, toon.rim.power, skin.floor],
            flags: [bits, kind, alpha_mode, u32::try_from(material.overlays.len()).unwrap_or(0)],
            alpha: [mask_cutoff, 0.0, 0.0, 0.0],
            textures: [texture_bits, 0, 0, 0],
            overlays,
            program,
            uv_transform: [
                material.uv_transform.scale.x,
                material.uv_transform.scale.y,
                material.uv_transform.offset.x,
                material.uv_transform.offset.y,
            ],
            outline_mix: [material.outline_style.lighting_mix, 0.0, 0.0, 0.0],
            ..mtoon_uniform(mtoon_of(material), texture_flags)
        };
        fx_uniform(material.fx.as_ref(), &mut uniform);
        uniform
    }
}

/// The `MToon` part of the uniform; all zeros (`MToon` off) for `None`. The rest of the uniform is left zeroed for the
/// caller to fill.
fn mtoon_uniform(m: Option<&Mtoon>, texture_flags: impl Fn(Option<TextureId>) -> u32) -> MaterialUniform {
    let mut uniform = MaterialUniform::zeroed();
    let Some(m) = m else { return uniform };
    let mut bits = mtoon::ON;
    let mut slot_flags = 0;
    let present = [
        mtoon::SHADE_TEXTURE,
        mtoon::SHIFT_TEXTURE,
        mtoon::MATCAP_TEXTURE,
        mtoon::RIM_TEXTURE,
        mtoon::EMISSIVE_TEXTURE,
        mtoon::NORMAL_TEXTURE,
        mtoon::MASK_TEXTURE,
    ];
    for ((i, id), bit) in (0u32..).zip(mtoon_slots(m)).zip(present) {
        if id.is_some() {
            bits |= bit;
        }
        slot_flags |= texture_flags(id) << (4 * i);
    }
    uniform.mtoon_flags = [bits, slot_flags, 0, 0];
    uniform.mtoon_shade = rgb(m.shade, m.shading_shift_scale);
    uniform.mtoon_shading = [m.shading_shift, m.shading_toony, m.gi_equalisation, m.rim_lighting_mix];
    uniform.mtoon_rim = rgb(m.rim, m.rim_fresnel_power);
    uniform.mtoon_matcap = rgb(m.matcap, m.rim_lift);
    uniform.mtoon_uv = [m.uv_scroll.x, m.uv_scroll.y, m.uv_rotation, m.normal_scale];
    uniform
}

/// A material on the GPU.
pub(crate) struct GpuMaterial {
    /// The CPU copy: the dependency check on release reads its texture list, and `update_material` replaces it.
    pub(crate) material: Material,
    uniform: wgpu::Buffer,
    /// Group 1 of `shader/common.wgsl`.
    pub(crate) bind_group: wgpu::BindGroup,
    /// The registered shading program the material draws with (an index into the registry), if any.
    pub(crate) program: Option<usize>,
}

impl GpuMaterial {
    /// Whether an instance drawn with this material gets an outline hull: the one rule the outline pass, the dynamic
    /// refresh and the morph pool share. A hull of no thickness would still leave a hairline where its back faces meet
    /// the front ones at the silhouette, so a 0-wide outline is none.
    pub(crate) fn draws_outline(&self) -> bool {
        self.material.outline.is_some_and(|outline| outline.width > 0.0)
    }

    /// Builds the uniform and bind group for `material`, whose textures the caller has checked are live.
    pub(crate) fn create(shared: &Shared, textures: &Arena<GpuTexture>, material: &Material) -> Self {
        use wgpu::util::DeviceExt;
        let uniform = shared.gpu.device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("material"),
            contents: bytemuck::bytes_of(&MaterialUniform::new(material, textures)),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        });
        let bind_group = bind_group(shared, &uniform, textures, material);
        Self {
            material: material.clone(),
            uniform,
            bind_group,
            program: program_of(&material.shading).map(|r| r.index),
        }
    }

    /// Replaces the contents in place: the uniform is rewritten and the bind group re-pointed at the new textures.
    pub(crate) fn update(&mut self, shared: &Shared, textures: &Arena<GpuTexture>, material: &Material) {
        shared.gpu.queue.write_buffer(&self.uniform, 0, bytemuck::bytes_of(&MaterialUniform::new(material, textures)));
        self.bind_group = bind_group(shared, &self.uniform, textures, material);
        self.material = material.clone();
        self.program = program_of(&material.shading).map(|r| r.index);
    }
}

fn bind_group(
    shared: &Shared,
    uniform: &wgpu::Buffer,
    textures: &Arena<GpuTexture>,
    material: &Material,
) -> wgpu::BindGroup {
    // Slot 0 is the base texture, slots 1..=4 the overlays; an empty slot reads the shared white texture.
    let slots: [Option<TextureId>; MATERIAL_TEXTURES as usize] = std::array::from_fn(|i| match i {
        0 => material.base_texture,
        _ => material.overlays.get(i - 1).map(|o| o.texture),
    });
    let found: Vec<Option<&GpuTexture>> =
        slots.iter().map(|id| id.and_then(|id| textures.get(id.index(), id.generation()))).collect();
    let mut entries = vec![wgpu::BindGroupEntry { binding: 0, resource: uniform.as_entire_binding() }];
    for (slot, texture) in (0u32..).zip(&found) {
        let view = texture.map_or(&shared.defaults.white, |t| &t.view);
        entries.push(wgpu::BindGroupEntry { binding: 1 + slot, resource: wgpu::BindingResource::TextureView(view) });
    }
    for (slot, texture) in (0u32..).zip(&found) {
        let sampler = texture.map_or(&shared.defaults.sampler, |t| &t.sampler);
        entries.push(wgpu::BindGroupEntry {
            binding: 1 + MATERIAL_TEXTURES + slot,
            resource: wgpu::BindingResource::Sampler(sampler),
        });
    }
    // The outline width texture; without one the shader does not read the slot, which holds the white stand-in.
    let width = material.outline_style.width_texture.and_then(|id| textures.get(id.index(), id.generation()));
    entries.push(wgpu::BindGroupEntry {
        binding: OUTLINE_WIDTH_BINDING,
        resource: wgpu::BindingResource::TextureView(width.map_or(&shared.defaults.white, |t| &t.view)),
    });
    entries.push(wgpu::BindGroupEntry {
        binding: OUTLINE_WIDTH_BINDING + 1,
        resource: wgpu::BindingResource::Sampler(width.map_or(&shared.defaults.sampler, |t| &t.sampler)),
    });
    // The `MToon` textures; an empty slot (or a material without `MToon`) holds the white stand-in, which the shader
    // reads only where white is the neutral value.
    let mtoon: Vec<Option<&GpuTexture>> = mtoon_of(material)
        .map(mtoon_slots)
        .unwrap_or_default()
        .iter()
        .map(|id| id.and_then(|id| textures.get(id.index(), id.generation())))
        .collect();
    for (slot, texture) in (0u32..).zip(&mtoon) {
        entries.push(wgpu::BindGroupEntry {
            binding: MTOON_BINDING + slot,
            resource: wgpu::BindingResource::TextureView(texture.map_or(&shared.defaults.white, |t| &t.view)),
        });
    }
    for (slot, texture) in (0u32..).zip(&mtoon) {
        entries.push(wgpu::BindGroupEntry {
            binding: MTOON_BINDING + MTOON_TEXTURES + slot,
            resource: wgpu::BindingResource::Sampler(texture.map_or(&shared.defaults.sampler, |t| &t.sampler)),
        });
    }
    shared.gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("material"),
        layout: &shared.layouts.material,
        entries: &entries,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_uniform_is_a_multiple_of_sixteen_bytes() {
        // std140: uniform struct sizes round up to 16. 9 vec4 + 4 overlays of 2 vec4 + 16 vec4 of program parameters + the
        // UV transform + the outline mix + 6 of MToon + 5 of surface fx = 46 vec4.
        assert_eq!(size_of::<MaterialUniform>(), 46 * 16);
        assert_eq!(size_of::<OverlayUniform>(), 32);
    }

    #[test]
    fn shader_constants_match_the_rust_ones() {
        let source = include_str!("../shader/common.wgsl");
        for (name, value) in [
            ("FLAG_DOUBLE_SIDED", flag::DOUBLE_SIDED),
            ("FLAG_VERTEX_COLOURS", flag::VERTEX_COLOURS),
            ("FLAG_RECEIVES_SHADOWS", flag::RECEIVES_SHADOWS),
            ("FLAG_SKIN", flag::SKIN),
            ("FLAG_BASE_TEXTURE", flag::BASE_TEXTURE),
            ("FLAG_OUTLINE", flag::OUTLINE),
            ("FLAG_OUTLINE_WORLD", flag::OUTLINE_WORLD),
            ("FLAG_OUTLINE_SCREEN", flag::OUTLINE_SCREEN),
            ("FLAG_OUTLINE_WIDTH_TEXTURE", flag::OUTLINE_WIDTH_TEXTURE),
            ("SHADING_TOON", shading::TOON),
            ("SHADING_UNLIT", shading::UNLIT),
            ("ALPHA_OPAQUE", alpha::OPAQUE),
            ("ALPHA_MASK", alpha::MASK),
            ("ALPHA_BLEND", alpha::BLEND),
            ("MTOON_ON", mtoon::ON),
            ("MTOON_SHADE_TEXTURE", mtoon::SHADE_TEXTURE),
            ("MTOON_SHIFT_TEXTURE", mtoon::SHIFT_TEXTURE),
            ("MTOON_MATCAP_TEXTURE", mtoon::MATCAP_TEXTURE),
            ("MTOON_RIM_TEXTURE", mtoon::RIM_TEXTURE),
            ("MTOON_EMISSIVE_TEXTURE", mtoon::EMISSIVE_TEXTURE),
            ("MTOON_NORMAL_TEXTURE", mtoon::NORMAL_TEXTURE),
            ("MTOON_MASK_TEXTURE", mtoon::MASK_TEXTURE),
        ] {
            let line = format!("const {name}: u32 = {value}u;");
            assert!(source.contains(&line), "common.wgsl should declare `{line}`");
        }
    }
}
