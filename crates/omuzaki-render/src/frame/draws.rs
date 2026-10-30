//! The frame's draw list and the GPU buffers behind it.
//!
//! [`FrameState::prepare`] is the single place a scene's handles are checked and turned into slots. After it succeeds
//! every [`Draw`] names a live mesh and material, so passes draw without further checks.
//!
//! There is **one** list, in scene order. Each [`Draw`] carries the set of passes that want it, decided once in
//! `prepare` from the contract's own fields ([`passes_of`]): `Opaque` for opaque and masked materials, `Transparent`
//! for blended ones, `Outlines` for materials with an outline, `Shadows` for instances that cast. A pass takes its
//! subset with [`Draws::for_pass`] and keeps whatever it derives from it (a sort order, a fitted frustum) in its own
//! module. Adding a pass's draws therefore never edits this file, and two passes never edit the same list.

use omuzaki_core::glam::{Mat4, UVec2};
use omuzaki_render_api::{AlphaMode, Instance, Material, Pass, RenderError, Scene, Sprite};

use super::supersample::{self, MAX_ROUNDS};
use super::uniform::{self, FrameUniform, InstanceUniform};
use crate::resource::SceneResources;
use crate::shared::{Layouts, Shared};

/// A set of passes, one bit per [`Pass`]. Nine passes fit sixteen bits.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct PassSet(u16);

const _: () = assert!(Pass::COUNT <= 16, "PassSet holds one bit per pass");

impl PassSet {
    /// The set plus `pass`.
    fn with(self, pass: Pass) -> Self {
        Self(self.0 | (1u16 << pass.index()))
    }

    fn contains(self, pass: Pass) -> bool {
        self.0 & (1u16 << pass.index()) != 0
    }
}

/// One mesh drawn with one material as one instance of the frame.
#[derive(Copy, Clone, Debug)]
pub(crate) struct Draw {
    /// Index into the frame's instance buffer (the scene's instance order).
    pub(crate) instance: u32,
    /// Slot of the mesh in the mesh table.
    pub(crate) mesh: u32,
    /// Slot of the material in the material table.
    pub(crate) material: u32,
    /// The transform flips winding, so the front face is clockwise.
    pub(crate) mirrored: bool,
    /// The passes that draw this instance.
    passes: PassSet,
}

/// The passes that draw `instance` with `material`: the one place the contract's per-instance rules turn into lists.
fn passes_of(material: &Material, instance: &Instance) -> PassSet {
    let mut passes = PassSet::default();
    // Opaque and masked instances draw in `Opaque` (which discards masked texels); blended ones in `Transparent`. A
    // masked, order-independent one draws in both: its soft edge, the texels the mask cuts, blends in `Transparent`.
    passes = passes.with(if matches!(material.alpha, AlphaMode::Blend) { Pass::Transparent } else { Pass::Opaque });
    if matches!(material.alpha, AlphaMode::Mask { .. }) && material.order_independent {
        passes = passes.with(Pass::Transparent);
    }
    if material.outline.is_some() {
        passes = passes.with(Pass::Outlines);
    }
    if instance.casts_shadow {
        passes = passes.with(Pass::Shadows);
    }
    passes
}

/// The frame's draws, rebuilt each frame into a retained vector (no allocation once it has grown to the scene's size).
#[derive(Default)]
pub(crate) struct Draws {
    /// Every instance of the scene that names live resources, in scene order.
    all: Vec<Draw>,
    /// Slot of the environment the scene is lit with, if it names one (checked live in `prepare`).
    pub(crate) environment: Option<u32>,
    /// The particle slots the scene's spawn batches submit (the sum of their `count`); every batch names a live system.
    pub(crate) particle_slots: u32,
}

impl Draws {
    /// The draws `pass` is responsible for, in scene order. Sorting (blended instances back to front, say) is the
    /// pass's own business and belongs in the pass's module.
    pub(crate) fn for_pass(&self, pass: Pass) -> impl Iterator<Item = &Draw> + Clone {
        self.all.iter().filter(move |draw| draw.passes.contains(pass))
    }
}

/// The storage buffers behind group 0, sized for the largest frame so far and replaced (with the bind groups) only when a
/// frame needs more.
struct FrameBuffers {
    instances: wgpu::Buffer,
    instance_capacity: usize,
    joints: wgpu::Buffer,
    joint_capacity: usize,
    /// Group 0 for each round: its own frame uniform, the shared instances and joints.
    bind_groups: Vec<wgpu::BindGroup>,
}

fn storage_buffer(device: &wgpu::Device, label: &str, capacity: usize, element: usize) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some(label),
        size: (capacity * element) as u64,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

impl FrameBuffers {
    fn new(
        device: &wgpu::Device,
        layouts: &Layouts,
        uniform: &FrameUniforms,
        instance_capacity: usize,
        joint_capacity: usize,
    ) -> Self {
        let instances = storage_buffer(device, "instances", instance_capacity, size_of::<InstanceUniform>());
        let joints = storage_buffer(device, "joints", joint_capacity, size_of::<Mat4>());
        let bind_groups = (0..u64::from(MAX_ROUNDS))
            .map(|round| {
                device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("frame"),
                    layout: &layouts.frame,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: wgpu::BindingResource::Buffer(uniform.round(round)),
                        },
                        wgpu::BindGroupEntry { binding: 1, resource: instances.as_entire_binding() },
                        wgpu::BindGroupEntry { binding: 2, resource: joints.as_entire_binding() },
                    ],
                })
            })
            .collect();
        Self { instances, instance_capacity, joints, joint_capacity, bind_groups }
    }
}

/// One frame uniform per supersampling round, `stride` bytes apart; a frame that does not supersample uses the first.
/// Made once with the renderer and never replaced, so a pass may keep a bind group of its own over it (`Particles` binds
/// it with a dynamic offset beside its own blocks).
pub(crate) struct FrameUniforms {
    pub(crate) buffer: wgpu::Buffer,
    pub(crate) stride: u64,
}

impl FrameUniforms {
    /// Bytes of one round's uniform.
    pub(crate) const SIZE: u64 = size_of::<FrameUniform>() as u64;

    fn new(device: &wgpu::Device) -> Self {
        let stride = Self::SIZE.next_multiple_of(u64::from(device.limits().min_uniform_buffer_offset_alignment));
        let buffer = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("frame uniform"),
            size: stride * u64::from(MAX_ROUNDS),
            usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        Self { buffer, stride }
    }

    fn round(&self, round: u64) -> wgpu::BufferBinding<'_> {
        wgpu::BufferBinding {
            buffer: &self.buffer,
            offset: round * self.stride,
            size: wgpu::BufferSize::new(Self::SIZE),
        }
    }
}

/// Everything about the current frame that outlives a single pass and is reused across frames.
pub(crate) struct FrameState {
    pub(crate) uniforms: FrameUniforms,
    buffers: FrameBuffers,
    pub(crate) draws: Draws,
    /// Instance records, built on the CPU first so validation finishes before any GPU write.
    instances: Vec<InstanceUniform>,
}

impl FrameState {
    pub(crate) fn new(device: &wgpu::Device, layouts: &Layouts) -> Self {
        let uniforms = FrameUniforms::new(device);
        let buffers = FrameBuffers::new(device, layouts, &uniforms, 1, 1);
        Self { uniforms, buffers, draws: Draws::default(), instances: Vec::new() }
    }

    /// Group 0 for each round of this frame (one unless it supersamples); valid once [`prepare`](Self::prepare) has
    /// succeeded for that many rounds.
    pub(crate) fn bind_groups(&self, rounds: u32) -> &[wgpu::BindGroup] {
        let rounds = (rounds.clamp(1, MAX_ROUNDS) as usize).min(self.buffers.bind_groups.len());
        &self.buffers.bind_groups[..rounds]
    }

    /// Validates `scene` against the resources, builds the draw lists and uploads the frame data: one frame uniform per
    /// round, each with its round's sub-pixel offset ([`supersample::jitter`]) for a GPU drawing `hardware` samples.
    ///
    /// # Errors
    /// `UnknownHandle` for a released mesh, material, environment or particle system; `Invalid` when an instance's joint
    /// range is too short for its mesh or a spawn batch lists a cell outside its system's atlas. Nothing has been written
    /// to the GPU when this returns an error.
    pub(crate) fn prepare(
        &mut self,
        shared: &Shared,
        scene: &Scene,
        resources: SceneResources<'_>,
        internal_size: UVec2,
        rounds: u32,
        hardware: u32,
    ) -> Result<(), RenderError> {
        self.draws.all.clear();
        self.instances.clear();
        self.draws.environment = None;
        self.draws.particle_slots = 0;
        if let Some(using) = &scene.environment {
            resources
                .environments
                .get(using.id.index(), using.id.generation())
                .ok_or(RenderError::UnknownHandle { what: "environment" })?;
            self.draws.environment = Some(using.id.index());
        }
        if let omuzaki_render_api::Background::Texture { texture, .. } = scene.background {
            resources
                .textures
                .get(texture.index(), texture.generation())
                .ok_or(RenderError::UnknownHandle { what: "texture" })?;
        }
        for batch in &scene.particles {
            let system = resources
                .particles
                .get(batch.system.index(), batch.system.generation())
                .ok_or(RenderError::UnknownHandle { what: "particle system" })?;
            if matches!(system.desc.sprite, Sprite::Atlas { .. })
                && batch.cells.as_slice().iter().any(|&cell| u32::from(cell) >= system.desc.cells())
            {
                return Err(RenderError::invalid("spawn batch", "a listed cell is outside its system's atlas"));
            }
            // At most `MAX_PARTICLES` in a valid scene.
            self.draws.particle_slots = self.draws.particle_slots.saturating_add(batch.count);
        }
        for (index, instance) in (0u32..).zip(&scene.instances) {
            let (mesh_slot, material_slot) = (instance.mesh.index(), instance.material.index());
            let mesh = resources
                .meshes
                .get(mesh_slot, instance.mesh.generation())
                .ok_or(RenderError::UnknownHandle { what: "mesh" })?;
            let material = resources
                .materials
                .get(material_slot, instance.material.generation())
                .ok_or(RenderError::UnknownHandle { what: "material" })?;
            let skinned = mesh.skinned && instance.skin.is_some();
            if let Some(range) = instance.skin.filter(|_| mesh.skinned)
                && mesh.joints_needed > range.len
            {
                let reason = format!(
                    "instance {index} skins with {} joints but its mesh weights joint {}",
                    range.len,
                    mesh.joints_needed - 1
                );
                return Err(RenderError::invalid("skin", reason));
            }
            self.instances.push(uniform::instance_uniform(instance, skinned));
            self.draws.all.push(Draw {
                instance: index,
                mesh: mesh_slot,
                material: material_slot,
                mirrored: uniform::mirrors(&instance.transform),
                passes: passes_of(&material.material, instance),
            });
        }

        let (device, queue) = (&shared.gpu.device, &shared.gpu.queue);
        self.reserve(device, &shared.layouts, self.instances.len(), scene.joints.len());
        let rounds = rounds.clamp(1, MAX_ROUNDS);
        for round in 0..rounds {
            let jitter = supersample::jitter(round, rounds, hardware);
            let packed = uniform::frame_uniform(scene, internal_size, jitter);
            queue.write_buffer(
                &self.uniforms.buffer,
                u64::from(round) * self.uniforms.stride,
                bytemuck::bytes_of(&packed),
            );
        }
        if !self.instances.is_empty() {
            queue.write_buffer(&self.buffers.instances, 0, bytemuck::cast_slice(&self.instances));
        }
        if !scene.joints.is_empty() {
            queue.write_buffer(&self.buffers.joints, 0, bytemuck::cast_slice(&scene.joints));
        }
        if let (Some(slot), Some(using)) = (self.draws.environment, &scene.environment)
            && let Some(environment) = resources.environments.at(slot)
        {
            environment.apply(queue, using);
        }
        Ok(())
    }

    /// Grows the storage buffers (and rebuilds the bind groups) if a frame needs more than they hold. Sizes round up to a
    /// power of two so a scene that grows one instance at a time reallocates a handful of times, not every frame.
    fn reserve(&mut self, device: &wgpu::Device, layouts: &Layouts, instances: usize, joints: usize) {
        let b = &self.buffers;
        if instances > b.instance_capacity || joints > b.joint_capacity {
            self.buffers = FrameBuffers::new(
                device,
                layouts,
                &self.uniforms,
                instances.max(b.instance_capacity).next_power_of_two(),
                joints.max(b.joint_capacity).next_power_of_two(),
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use omuzaki_render_api::{Colour, MaterialId, MeshId, Outline};

    use super::*;

    fn instance(casts_shadow: bool) -> Instance {
        let mut instance = Instance::new(MeshId::from_parts(0, 0), MaterialId::from_parts(0, 0), Mat4::IDENTITY);
        instance.casts_shadow = casts_shadow;
        instance
    }

    /// The passes in `set`, in frame order.
    fn listed(set: PassSet) -> Vec<Pass> {
        Pass::ALL.into_iter().filter(|&p| set.contains(p)).collect()
    }

    #[test]
    fn an_opaque_caster_is_drawn_by_opaque_and_shadows() {
        let material = Material::toon(Colour::WHITE);
        assert_eq!(listed(passes_of(&material, &instance(true))), [Pass::Shadows, Pass::Opaque]);
        assert_eq!(listed(passes_of(&material, &instance(false))), [Pass::Opaque], "no shadow if it does not cast");
    }

    #[test]
    fn masked_materials_draw_with_the_opaque_pass_and_blended_ones_with_the_transparent_pass() {
        let mut material = Material::toon(Colour::WHITE);
        material.alpha = AlphaMode::Mask { cutoff: 0.5 };
        assert_eq!(listed(passes_of(&material, &instance(false))), [Pass::Opaque]);
        material.alpha = AlphaMode::Blend;
        assert_eq!(listed(passes_of(&material, &instance(false))), [Pass::Transparent]);
    }

    #[test]
    fn a_soft_edged_mask_draws_its_body_opaque_and_its_edge_blended() {
        let mut material = Material::toon(Colour::WHITE);
        material.alpha = AlphaMode::Mask { cutoff: 0.5 };
        material.order_independent = true;
        assert_eq!(listed(passes_of(&material, &instance(false))), [Pass::Opaque, Pass::Transparent]);
        material.alpha = AlphaMode::Opaque;
        assert_eq!(listed(passes_of(&material, &instance(false))), [Pass::Opaque], "an opaque surface has no edge");
    }

    #[test]
    fn an_outline_adds_the_outline_pass_whatever_the_alpha_mode() {
        let mut material = Material::toon(Colour::WHITE);
        material.outline = Some(Outline { colour: Colour::BLACK, width: 0.01 });
        assert_eq!(listed(passes_of(&material, &instance(false))), [Pass::Opaque, Pass::Outlines]);
        material.alpha = AlphaMode::Blend;
        assert_eq!(
            listed(passes_of(&material, &instance(true))),
            [Pass::Shadows, Pass::Outlines, Pass::Transparent],
            "the mock outlines blended instances too"
        );
    }

    #[test]
    fn passes_that_do_not_draw_instances_never_get_any() {
        let mut material = Material::toon(Colour::WHITE);
        material.outline = Some(Outline { colour: Colour::BLACK, width: 0.01 });
        let set = passes_of(&material, &instance(true));
        for pass in [Pass::PaintTextures, Pass::Bloom, Pass::DepthOfField, Pass::Resolve] {
            assert!(!set.contains(pass), "{} works on the frame, not on instances", pass.name());
        }
    }

    #[test]
    fn for_pass_filters_one_list_in_scene_order() {
        let draw = |instance: u32, pass: Pass| Draw {
            instance,
            mesh: 0,
            material: 0,
            mirrored: false,
            passes: PassSet::default().with(pass),
        };
        let draws = Draws {
            all: vec![
                draw(0, Pass::Opaque),
                draw(1, Pass::Transparent),
                draw(2, Pass::Opaque),
                draw(3, Pass::Outlines),
            ],
            environment: None,
            particle_slots: 0,
        };
        let of = |pass| draws.for_pass(pass).map(|d| d.instance).collect::<Vec<_>>();
        assert_eq!(
            (of(Pass::Opaque), of(Pass::Transparent), of(Pass::Outlines), of(Pass::Shadows)),
            (vec![0, 2], vec![1], vec![3], vec![])
        );
    }
}
