//! The plumbing every render job shares: the size limits, the cost hint, building the character from the document, the
//! light rig, and storing the PNG under its `omuzaki://render/` name. A job op uses these rather than its own copies, so
//! every render is limited, costed, built and stored the same way.

use std::sync::Mutex;

use omuzaki_char_api::{
    BuildOptions, BuildServices, BuiltCharacter, CharId, CharacterBuilder, CharacterDef, Quality as BuildQuality,
    VariantError, VariantId, VariantSet, paths, resolve_variant,
};
use omuzaki_core::glam::UVec2;
use omuzaki_doc_api::DocRead;
use omuzaki_ops_api::{CostHint, JobCtx, OpError};
use omuzaki_render_api::{Quality, Renderer, ShadowQuality};
use omuzaki_store_api::{Commercial, ContentHash, Provenance, Source, Store};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::still::StillLight;
use crate::error::StageError;
use crate::recipe::LightRig;

/// The smallest side of a render, pixels.
pub const MIN_SIDE: u32 = 16;
/// The largest side of a render, pixels.
pub const MAX_SIDE: u32 = 4096;
/// The light rig a render uses when none is named.
pub const DEFAULT_LIGHT_RIG: &str = "viewer-studio";

/// How much work a render may spend: the render-api [`Quality`] presets. Offline is the default, because it is the one
/// whose bytes are the same on every run.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RenderQuality {
    /// Live-preview settings: fast.
    Live,
    /// More samples than live.
    High,
    /// Frame-exact offline settings: the same bytes on every run.
    #[default]
    Offline,
}

impl RenderQuality {
    /// The render settings for this preset.
    #[must_use]
    pub fn quality(self) -> Quality {
        match self {
            Self::Live => Quality::live(),
            Self::High => Quality::high(),
            Self::Offline => Quality::offline(),
        }
    }

    /// The character's level of detail. High uses the offline build too: a denser mesh than the live one is what
    /// `High` asks for.
    #[must_use]
    pub fn detail(self) -> BuildQuality {
        match self {
            Self::Live => BuildQuality::Live,
            Self::High | Self::Offline => BuildQuality::Offline,
        }
    }
}

/// `size` as a target size.
///
/// # Errors
/// `Invalid` for field `size`, naming the limits, when a side is outside [`MIN_SIDE`]`..=`[`MAX_SIDE`].
pub fn check_size(size: [u32; 2]) -> Result<UVec2, OpError> {
    let [x, y] = size;
    let within = |side: u32| (MIN_SIDE..=MAX_SIDE).contains(&side);
    if within(x) && within(y) {
        return Ok(UVec2::new(x, y));
    }
    Err(OpError::invalid("size", format!("size {x}x{y} is outside the limits: each side is {MIN_SIDE} to {MAX_SIDE} pixels"))
        .hint("pick a size between the limits; the offline quality is slowest at the largest"))
}

/// One mebibyte, in bytes.
const MIB: u64 = 1 << 20;
/// MiB the character's own GPU data takes (meshes and textures), added to a render's images. An allowance, not a
/// measurement.
const CHARACTER_MIB: u64 = 64;

// The sizes and formats below mirror `omuzaki-render`, which the stage does not depend on. Change them together with the
// renderer: the test `the_cost_hint_counts_every_image_the_render_allocates` pins the arithmetic.
/// Bytes of a pixel of an `Rgba16Float` image: the scene colour and the bloom images (`gpu.rs`, `SCENE_FORMAT`).
const HALF_RGBA_BYTES: u64 = 8;
/// Bytes of a pixel of the output: `Rgba8Unorm` (`gpu.rs`, `OUTPUT_FORMAT`).
const OUTPUT_BYTES: u64 = 4;
/// Bytes of a depth texel: `Depth32Float` (`gpu.rs`, `DEPTH_FORMAT`), for the scene depth and the shadow map.
const DEPTH_BYTES: u64 = 4;
/// Bloom's blurred mips (`pass/bloom.rs`, `MIPS`); each has a horizontal and a vertical image.
const BLOOM_MIPS: u64 = 5;
/// Rows of a read-back copy are padded to this many bytes (`wgpu::COPY_BYTES_PER_ROW_ALIGNMENT`).
const COPY_ROW_ALIGNMENT: u64 = 256;
/// Bloom chains the renderer keeps across frames, one per target size, the oldest dropped (`pass/bloom.rs`,
/// `CACHED_SIZES`). Every job on a renderer shares them, so any job's peak can include all of them.
const BLOOM_CACHED_CHAINS: u64 = 3;
/// The largest side a job renders, so the largest target a bloom chain is built for. A job's `size` is refused above it
/// (`check_size`), so no cached chain is larger.
const MAX_TARGET_SIDE: u32 = MAX_SIDE;

/// The bytes a pixel of a render's scene attachments takes at `samples` samples: 8 of colour per sample, the resolved
/// colour when multisampled, and 4 of depth per sample (`frame/attachments.rs`, `bytes`).
fn attachment_bytes_per_pixel(samples: u64) -> u64 {
    8 + if samples > 1 { 8 * samples } else { 0 } + DEPTH_BYTES * samples
}

/// The side, in texels, of the shadow map a shadow quality allocates (`shadow/map.rs`, `settings`). Zero when off.
fn shadow_side(shadows: ShadowQuality) -> u64 {
    match shadows {
        ShadowQuality::Off => 0,
        ShadowQuality::Low => 1024,
        ShadowQuality::High => 2048,
    }
}

/// Half of a side, rounded up and never below one (`pass/bloom.rs`, `half`).
fn half(side: u64) -> u64 {
    side.div_ceil(2).max(1)
}

/// The bloom chain's bytes at a target of `target` pixels (`pass/bloom.rs`): the bright image at half the target, then
/// each of the five mips at half the one before, and two images per mip. Bloom works at the target's size, whatever the
/// render scale.
fn bloom_bytes(target: UVec2) -> u64 {
    let (mut width, mut height) = (half(u64::from(target.x)), half(u64::from(target.y)));
    let bright = width * height;
    let mut mips = 0;
    for _ in 0..BLOOM_MIPS {
        mips += 2 * width * height;
        (width, height) = (half(width), half(height));
    }
    (bright + mips) * HALF_RGBA_BYTES
}

/// The cost hint of `images` stills of `size`: an upper bound for the GPU memory a render's images take at peak, plus
/// [`CHARACTER_MIB`] for the character's meshes and textures. It counts, at the preset's sizes and formats:
///
/// - the scene attachments at the internal size (`size` times the preset's render scale, rounded up), with the preset's
///   sample count, the most it asks for, so the renderer's own choice can only be lower;
/// - the shadow map, at the preset's shadow quality, though a still may have no casting light;
/// - the output, RGBA8 at `size`;
/// - the read-back staging buffer, RGBA8 at `size` with rows padded to 256 bytes;
/// - the bloom chains the renderer keeps: [`BLOOM_CACHED_CHAINS`] chains, each at the largest target a job renders
///   ([`MAX_TARGET_SIDE`]). The cache keeps chains from earlier jobs whatever this job's preset, so this is charged for
///   every preset, and it is the cache's worst case: the job's own chain is one of its three slots.
///
/// Not counted: depth of field, whose layers are cached the same way but which no still enables (`LightRig::post`);
/// environment maps, which a still's scene never names; and the morph buffers, which are character data and so fall in
/// the [`CHARACTER_MIB`] allowance with the meshes and textures.
///
/// Worked: a 4096 by 4096 high still at 4 samples. Its internal size is 6144 by 6144, at 56 bytes a pixel: 2016 MiB of
/// attachments. The shadow map is 2048 by 2048 at 4 bytes: 16 MiB. The output is 64 MiB and the read-back 64 MiB. The
/// bloom chain of a 4096 target is 117 MiB, and three of them are 352 MiB. Together with the 64 MiB allowance, the total
/// is 2576 MiB.
///
/// Seconds per still: 0.2 live, 0.5 high, 2.0 offline. No money is spent. Setting `gpu_mib` puts the job on the GPU
/// lane.
#[must_use]
#[allow(
    clippy::cast_precision_loss,
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    reason = "a job's image count stays far below 2^24, and a rounded-up pixel count is a non-negative whole number"
)]
pub fn cost(size: UVec2, images: u32, quality: RenderQuality) -> CostHint {
    let preset = quality.quality();
    let internal = |side: u32| (f64::from(side) * f64::from(preset.render_scale)).ceil() as u64;
    let target = u64::from(size.x) * u64::from(size.y);
    let attachments = internal(size.x) * internal(size.y) * attachment_bytes_per_pixel(u64::from(preset.samples));
    let shadow = shadow_side(preset.shadows).pow(2) * DEPTH_BYTES;
    let output = target * OUTPUT_BYTES;
    let readback = (u64::from(size.x) * OUTPUT_BYTES).next_multiple_of(COPY_ROW_ALIGNMENT) * u64::from(size.y);
    let largest = UVec2::splat(MAX_TARGET_SIDE);
    let bloom = BLOOM_CACHED_CHAINS * bloom_bytes(largest);
    let bytes = attachments + shadow + output + readback + bloom;
    let mib = u32::try_from(bytes.div_ceil(MIB) + CHARACTER_MIB).unwrap_or(u32::MAX);
    let per_image = match quality {
        RenderQuality::Live => 0.2,
        RenderQuality::High => 0.5,
        RenderQuality::Offline => 2.0,
    };
    CostHint { seconds: Some(images as f32 * per_image), gpu_mib: Some(mib), usd: None }
}

/// The light rig `name` as a still's light.
///
/// # Errors
/// `Invalid` for field `light_rig`, listing the rigs, when there is no rig called `name`.
pub fn still_light(name: &str) -> Result<StillLight, OpError> {
    if !LightRig::names().any(|known| known == name) {
        let known: Vec<&str> = LightRig::names().collect();
        return Err(OpError::invalid("light_rig", format!("there is no light rig '{name}'"))
            .hint(format!("the light rigs are {}", known.join(", "))));
    }
    let rig = LightRig::load(name).map_err(|e| OpError::internal(format!("the light rig '{name}' doesn't load: {e}")))?;
    let lights = rig.to_lights().map_err(|e| OpError::internal(format!("the light rig '{name}' has no lights: {e}")))?;
    Ok(StillLight::new(lights, rig.background(), rig.post(true)))
}

/// Character `id` from the job's document, with `variant` applied, built by the injected builder.
///
/// # Errors
/// `NotFound` for an unknown character or variant; `Invalid` for a character that doesn't read or build;
/// `Unavailable` when the builder or its services are not injected.
pub fn build_character(
    job: &JobCtx<'_>,
    id: &CharId,
    variant: Option<&VariantId>,
    quality: RenderQuality,
) -> Result<BuiltCharacter, OpError> {
    let builder = job.service::<dyn CharacterBuilder>()?;
    let services = job.service::<BuildServices>()?;
    let def = job.read(|doc| read_definition(doc, id, variant))?;
    let options = BuildOptions { variant: variant.cloned(), quality: quality.detail(), rig_export: false };
    builder
        .build(id, &def, &options, services)
        .map_err(|e| OpError::invalid("character", format!("character '{id}' doesn't build: {e}")))
}

/// The definition of `id` with `variant` applied, read from the document.
pub(crate) fn read_definition(doc: &dyn DocRead, id: &CharId, variant: Option<&VariantId>) -> Result<CharacterDef, OpError> {
    let node = doc.get(&paths::character(id)).ok_or_else(|| {
        OpError::not_found(format!("there is no character '{id}'"))
            .hint("check the id; characters live under characters/<id>")
    })?;
    let base = CharacterDef::from_node(node)
        .map_err(|e| OpError::invalid("character", format!("character '{id}' doesn't read as a character: {e}")))?;
    let Some(variant) = variant else { return Ok(base) };
    let variants = read_variants(doc, id)?;
    resolve_variant(&base, &variants, variant).map_err(|e| variant_error(id, variant, e))
}

/// The variants stored under `id`, or none when it has no variants table.
fn read_variants(doc: &dyn DocRead, id: &CharId) -> Result<VariantSet, OpError> {
    let Some(node) = doc.get(&paths::variants(id)) else { return Ok(VariantSet::new()) };
    serde_json::from_value::<VariantSet>(node.clone())
        .map_err(|e| OpError::invalid("variant", format!("the variants of '{id}' don't read: {e}")))
}

/// Maps a variant failure to an op error. A missing variant is the one asked for; a missing parent is named as the
/// parent, and as inherited through `extends` from the variant asked for.
fn variant_error(id: &CharId, variant: &VariantId, error: VariantError) -> OpError {
    match error {
        VariantError::Unknown(missing) if &missing == variant => {
            OpError::not_found(format!("there is no variant '{variant}' on '{id}'"))
        }
        VariantError::Unknown(parent) => OpError::not_found(format!(
            "variant '{variant}' inherits from '{parent}' through extends, and '{parent}' isn't a variant of '{id}'"
        ))
        .hint("create the parent variant, or change what the variant extends"),
        other => OpError::invalid("variant", other.to_string()),
    }
}

/// Lends the renderer service to `f`. The renderer is injected as `Mutex<Box<dyn Renderer>>`.
///
/// # Errors
/// `Unavailable` when no renderer is injected; `Internal` when the lock is poisoned (an earlier render panicked while
/// holding it) or when `f` fails, with the stage's message.
pub fn with_renderer<T>(
    job: &JobCtx<'_>,
    f: impl FnOnce(&mut dyn Renderer) -> Result<T, StageError>,
) -> Result<T, OpError> {
    let renderer = job.service::<Mutex<Box<dyn Renderer>>>()?;
    let mut guard = renderer
        .lock()
        .map_err(|_| OpError::internal("the renderer is unusable: an earlier render panicked while holding it"))?;
    f(&mut **guard).map_err(|e| OpError::internal(e.to_string()))
}

/// A PNG in the asset store, by its content hash.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct StoredRender {
    /// The content hash of the PNG.
    pub hash: ContentHash,
    /// `omuzaki://render/<hash>.png`, which `omuzaki_mcp::RenderProvider` serves (#1476).
    pub uri: String,
}

/// The provenance of a render: authored by the user, under `LicenseRef-Omuzaki-User`, with commercial status unknown
/// until the render licence gate (scenes design S23) derives it from the cast. It names no author, attribution or
/// consent, and it is not private test material.
#[must_use]
pub fn render_provenance() -> Provenance {
    Provenance {
        source: Source::Authored,
        author: None,
        licence: "LicenseRef-Omuzaki-User".to_owned(),
        attribution: None,
        commercial: Commercial::Unknown,
        consent: None,
        private_test: false,
    }
}

/// Puts `png` in the store with [`render_provenance`].
///
/// # Errors
/// `Unavailable` when no store is injected; `Internal` when the store refuses the write.
pub fn store_png(job: &JobCtx<'_>, png: &[u8]) -> Result<StoredRender, OpError> {
    let store = job.service::<dyn Store>()?;
    let hash = store
        .put(png, render_provenance())
        .map_err(|e| OpError::internal(format!("storing the PNG failed: {e}")))?;
    Ok(StoredRender { uri: format!("omuzaki://render/{hash}.png"), hash })
}
