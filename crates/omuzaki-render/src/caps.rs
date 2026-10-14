//! Capability profiles: the named limit sets the renderer opens its device with, and the list of what it needs from them.
//!
//! The renderer never asks an adapter what it can do and then asks for all of it (`Adapter::limits`): on the dev
//! machine's 4090 that lets any use of a feature a laptop lacks pass silently. It asks for a *named* set instead,
//! so a feature the profile does not grant fails on the dev machine too, and CI proves a profile by opening it on
//! lavapipe.
//!
//! wgpu reports only the first limit a pipeline or layout breaks. [`REQUIREMENTS`] therefore states what the renderer
//! needs as data, and [`unmet`] checks all of it at once, so a profile that cannot work fails with the whole list.

use std::fmt;
use std::str::FromStr;

use omuzaki_render_api::RenderError;
use omuzaki_render_api::limits::{GUARANTEED_TEXTURE_SIZE, MAX_JOINTS};

use crate::frame::uniform::FrameUniform;
use crate::resource::material::MaterialUniform;
use crate::shared::MATERIAL_TEXTURES;

/// The environment variable that picks a profile for any program (a test, a bench, the CLI): `native`, `webgpu` or
/// `webgl2`. Unset means [`CapabilityProfile::Native`].
pub const PROFILE_ENV: &str = "OMUZAKI_RENDER_PROFILE";

/// A named set of device limits and features the renderer opens its device with (spec v3 §10).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash)]
pub enum CapabilityProfile {
    /// The desktop application: WebGPU's default limits (the same floor as [`WebGpu`](Self::WebGpu), so any adapter that meets them
    /// opens), plus GPU timestamps when the adapter has them.
    #[default]
    Native,
    /// A browser with WebGPU: exactly `wgpu::Limits::default()`, the set every WebGPU browser guarantees, and only the
    /// timestamp feature browsers expose (`timestamp-query`, without timestamps between passes).
    WebGpu,
    /// A browser with only WebGL2 (the spec's web fallback): `wgpu::Limits::downlevel_webgl2_defaults()`, no optional
    /// features. It has no storage buffers or compute.
    WebGl2,
}

impl CapabilityProfile {
    /// Every profile, strongest first.
    pub const ALL: [Self; 3] = [Self::Native, Self::WebGpu, Self::WebGl2];

    /// The name used on the command line and in [`PROFILE_ENV`].
    #[must_use]
    pub const fn name(self) -> &'static str {
        match self {
            Self::Native => "native",
            Self::WebGpu => "webgpu",
            Self::WebGl2 => "webgl2",
        }
    }

    /// The limits the device is opened with: exactly these, whatever the adapter offers beyond them.
    #[must_use]
    pub const fn limits(self) -> wgpu::Limits {
        match self {
            Self::Native | Self::WebGpu => wgpu::Limits::defaults(),
            Self::WebGl2 => wgpu::Limits::downlevel_webgl2_defaults(),
        }
    }

    /// The optional features the profile may use. The device is given those of them the adapter has; the renderer
    /// works without any (timings are then absent).
    #[must_use]
    pub const fn features(self) -> wgpu::Features {
        match self {
            Self::Native => wgpu::Features::TIMESTAMP_QUERY.union(wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS),
            Self::WebGpu => wgpu::Features::TIMESTAMP_QUERY,
            Self::WebGl2 => wgpu::Features::empty(),
        }
    }

    /// The profile named by [`PROFILE_ENV`], or [`Native`](Self::Native) if it is unset.
    ///
    /// # Errors
    /// `Invalid` if the variable names no profile.
    pub fn from_env() -> Result<Self, RenderError> {
        match std::env::var(PROFILE_ENV) {
            Ok(value) => value.parse(),
            Err(_) => Ok(Self::default()),
        }
    }

    /// Everything in [`REQUIREMENTS`] that `limits` does not meet; empty if the renderer can run on them.
    #[must_use]
    pub fn unmet_requirements(self) -> Vec<Unmet> {
        unmet(&self.limits())
    }
}

impl fmt::Display for CapabilityProfile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

impl FromStr for CapabilityProfile {
    type Err = RenderError;

    fn from_str(text: &str) -> Result<Self, RenderError> {
        Self::ALL.into_iter().find(|p| p.name().eq_ignore_ascii_case(text.trim())).ok_or_else(|| {
            RenderError::invalid(
                "capability profile",
                format!("{text:?} is not one of native, webgpu, webgl2 (from {PROFILE_ENV})"),
            )
        })
    }
}

/// One thing the renderer needs from the device: a minimum for one limit, and why.
#[derive(Copy, Clone, Debug)]
pub struct Requirement {
    /// The wgpu limit, by its field name.
    pub limit: &'static str,
    /// The smallest value that works.
    pub needed: u64,
    /// What in the renderer needs it.
    pub why: &'static str,
    /// Reads the limit from a set.
    read: fn(&wgpu::Limits) -> u64,
}

/// A [`Requirement`] a limit set does not meet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Unmet {
    /// The wgpu limit, by its field name.
    pub limit: &'static str,
    /// What the renderer needs.
    pub needed: u64,
    /// What the limit set offers.
    pub available: u64,
    /// What in the renderer needs it.
    pub why: &'static str,
}

impl fmt::Display for Unmet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} needs {} but offers {} ({})", self.limit, self.needed, self.available, self.why)
    }
}

/// Builds a [`Requirement`] reading `$field`.
macro_rules! need {
    ($field:ident, $needed:expr, $why:expr) => {
        Requirement { limit: stringify!($field), needed: $needed as u64, why: $why, read: |l| u64::from(l.$field) }
    };
    ($field:ident as u64, $needed:expr, $why:expr) => {
        Requirement { limit: stringify!($field), needed: $needed as u64, why: $why, read: |l| l.$field }
    };
}

/// Bytes in one mesh vertex; a test keeps it equal to the real layout's stride.
const MESH_VERTEX_STRIDE: u64 = 72;

/// What the renderer needs from the device, as data. Each entry comes from a layout or constant in the crate (the
/// `why`), and the test `a_profile_that_meets_the_requirements_opens_a_renderer` keeps the list honest: every profile
/// it clears must open and render.
pub const REQUIREMENTS: &[Requirement] = &[
    need!(max_bind_groups, 4, "frame, material, shadow/params and per-pass groups 0 to 3"),
    need!(max_storage_buffers_per_shader_stage, 2, "frame group: instance records and joint matrices"),
    need!(
        max_storage_buffer_binding_size as u64,
        MAX_JOINTS * size_of::<omuzaki_core::glam::Mat4>(),
        "a full joint palette in one storage buffer binding (MAX_JOINTS matrices)"
    ),
    need!(max_uniform_buffer_binding_size as u64, size_of::<FrameUniform>(), "the frame uniform (camera and lights)"),
    need!(max_uniform_buffer_binding_size as u64, size_of::<MaterialUniform>(), "the material uniform"),
    need!(max_uniform_buffers_per_shader_stage, 4, "frame, material, shadow and pass parameters"),
    need!(max_sampled_textures_per_shader_stage, MATERIAL_TEXTURES + 1, "material textures and the shadow map"),
    need!(max_samplers_per_shader_stage, MATERIAL_TEXTURES + 1, "material samplers and the shadow comparison"),
    need!(max_vertex_buffers, 5, "the mesh stream and four extra attribute streams"),
    need!(max_bindings_per_bind_group, MATERIAL_TEXTURES * 2 + 1, "the material group's bindings 0 to 10"),
    need!(max_vertex_buffer_array_stride, MESH_VERTEX_STRIDE, "the mesh vertex (72 bytes)"),
    need!(max_vertex_attributes, 10, "six mesh attributes and four extra ones"),
    need!(max_texture_dimension_2d, GUARANTEED_TEXTURE_SIZE, "the texture size the render contract guarantees"),
    need!(max_color_attachments, 1, "every pass draws to at least one target"),
];

/// Everything in [`REQUIREMENTS`] that `limits` does not meet, in the order of the list. Empty if it meets all.
#[must_use]
pub fn unmet(limits: &wgpu::Limits) -> Vec<Unmet> {
    REQUIREMENTS
        .iter()
        .filter_map(|r| {
            let available = (r.read)(limits);
            (available < r.needed).then_some(Unmet { limit: r.limit, needed: r.needed, available, why: r.why })
        })
        .collect()
}

/// Turns a non-empty `unmet` list into the error a renderer returns when asked to open on `what`, naming every one.
///
/// # Errors
/// `Backend` listing all unmet requirements; `Ok` if there are none.
pub(crate) fn require(what: &str, limits: &wgpu::Limits) -> Result<(), RenderError> {
    let list = unmet(limits);
    if list.is_empty() {
        return Ok(());
    }
    let items: Vec<String> = list.iter().map(ToString::to_string).collect();
    Err(RenderError::Backend(format!(
        "{what} does not meet the renderer's requirements ({} unmet): {}",
        list.len(),
        items.join("; ")
    )))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profiles_parse_by_name_and_reject_anything_else() {
        for profile in CapabilityProfile::ALL {
            assert_eq!(profile.name().parse::<CapabilityProfile>().unwrap(), profile);
            assert_eq!(profile.name().to_uppercase().parse::<CapabilityProfile>().unwrap(), profile);
        }
        let Err(RenderError::Invalid { reason, .. }) = "gles".parse::<CapabilityProfile>() else {
            panic!("an unknown profile must be rejected")
        };
        assert!(reason.contains("\"gles\"") && reason.contains(PROFILE_ENV), "{reason}");
    }

    #[test]
    fn the_profile_limits_are_the_documented_sets() {
        assert_eq!(CapabilityProfile::WebGpu.limits(), wgpu::Limits::defaults());
        assert_eq!(CapabilityProfile::WebGl2.limits(), wgpu::Limits::downlevel_webgl2_defaults());
        assert_eq!(CapabilityProfile::Native.limits(), wgpu::Limits::defaults());
        assert_eq!(CapabilityProfile::Native.limits().max_texture_dimension_2d, 8192);
        assert_eq!(CapabilityProfile::default(), CapabilityProfile::Native);
        assert!(CapabilityProfile::WebGl2.features().is_empty());
    }

    #[test]
    fn the_webgpu_profiles_meet_every_requirement() {
        for profile in [CapabilityProfile::Native, CapabilityProfile::WebGpu] {
            assert_eq!(profile.unmet_requirements(), vec![], "{profile}");
        }
    }

    #[test]
    fn webgl2_lists_every_unmet_requirement_not_just_the_first() {
        let limits: Vec<&str> = CapabilityProfile::WebGl2.unmet_requirements().iter().map(|u| u.limit).collect();
        // Storage buffers (instances, joints): how many and how large; and the 2048-pixel texture limit under the contract's 4096.
        assert_eq!(
            limits,
            ["max_storage_buffers_per_shader_stage", "max_storage_buffer_binding_size", "max_texture_dimension_2d",]
        );
    }

    #[test]
    fn a_failing_profile_is_one_error_that_names_every_unmet_requirement() {
        let Err(RenderError::Backend(message)) = require("webgl2", &CapabilityProfile::WebGl2.limits()) else {
            panic!("webgl2 limits must be refused")
        };
        assert!(message.contains("3 unmet"), "{message}");
        for needle in [
            "max_storage_buffers_per_shader_stage needs 2 but offers 0",
            "max_texture_dimension_2d needs 4096 but offers 2048",
        ] {
            assert!(message.contains(needle), "{needle:?} missing from {message}");
        }
        assert_eq!(require("native", &CapabilityProfile::Native.limits()), Ok(()));
    }

    #[test]
    fn the_joint_palette_is_what_overflows_a_uniform_buffer() {
        // The decision numbers: 2048 joints are 128 KiB, a WebGL2 uniform binding holds 16 KiB (256 joints).
        let palette = MAX_JOINTS * size_of::<omuzaki_core::glam::Mat4>();
        let uniform = usize::try_from(CapabilityProfile::WebGl2.limits().max_uniform_buffer_binding_size).unwrap();
        assert_eq!((palette, uniform, uniform / size_of::<omuzaki_core::glam::Mat4>()), (131_072, 16_384, 256));
    }

    #[test]
    fn no_source_file_asks_the_adapter_for_its_limits() {
        // The call this crate must not make: it hands the device everything the dev machine has.
        let call = ["adapter", ".limits()"].concat();
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
        let mut stack = vec![dir];
        while let Some(path) = stack.pop() {
            for entry in std::fs::read_dir(&path).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.extension().is_some_and(|e| e == "rs") {
                    let text = std::fs::read_to_string(&path).unwrap();
                    assert!(!text.contains(&call), "{} asks the adapter for its limits", path.display());
                }
            }
        }
    }

    #[test]
    fn lowering_the_bind_group_and_vertex_stride_limits_together_reports_both() {
        assert_eq!(crate::resource::mesh::vertex_layout().array_stride, MESH_VERTEX_STRIDE);
        let limits = wgpu::Limits {
            max_bindings_per_bind_group: 10,
            max_vertex_buffer_array_stride: 64,
            ..wgpu::Limits::defaults()
        };
        let names: Vec<&str> = unmet(&limits).iter().map(|u| u.limit).collect();
        assert_eq!(names, ["max_bindings_per_bind_group", "max_vertex_buffer_array_stride"]);
    }
}
