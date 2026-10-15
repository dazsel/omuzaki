//! Loading from memory with ufbx: the settings that put every file in canonical space, the limits, and the mapping of
//! ufbx's errors onto [`IoError`].
//!
//! **Axes.** ufbx's `front` is the direction the model faces *toward the viewer*, the opposite of its `forward`. FBX
//! stores it as `FrontAxis` (and `CoordAxis` as `right`, `UpAxis` as `up`), so a Maya file with `FrontAxis = +Z` has a
//! character facing +Z, and a 3ds Max file with `FrontAxis = -Y` has one facing -Y. The target is ufbx's right-handed
//! Y-up, `right = +X`, `up = +Y`, `front = +Z`: the model's front toward +Z, as glTF and VRM 1.0 have it (io-api D2).

use omuzaki_io_api::{ImportCtx, IoError};
use ufbx::{
    AllocatorOpts, CoordinateAxes, CoordinateAxis, ErrorType, FileFormat, GeometryTransformHandling,
    InheritModeHandling, LoadOpts, Progress, ProgressCb, ProgressResult, SceneRoot, SpaceConversion,
    UnicodeErrorHandling,
};

/// The axes every import is converted to: right-handed, +Y up, the model's front toward +Z.
pub(crate) const TARGET_AXES: CoordinateAxes = CoordinateAxes {
    right: CoordinateAxis::PositiveX,
    up: CoordinateAxis::PositiveY,
    front: CoordinateAxis::PositiveZ,
};

/// The most memory ufbx may use for its working data, and again for the scene it returns, in bytes.
pub(crate) const MEMORY_LIMIT_BYTES: usize = 1 << 30;
/// The most allocations ufbx may make for each of those two, so a hostile file can't spend time on millions of tiny ones.
pub(crate) const ALLOCATION_LIMIT: usize = 32_000_000;
/// The deepest node hierarchy accepted. ufbx's default is unlimited, and a deep chain is a cheap way to exhaust a stack.
pub(crate) const NODE_DEPTH_LIMIT: u32 = 1024;

/// The progress stage name reported while ufbx parses.
const STAGE: &str = "fbx";

/// Reads `bytes` (already sniffed as FBX) with ufbx, converted to canonical space.
///
/// # Errors
/// [`IoError::Malformed`] for a parse failure, [`IoError::Limit`] over a ufbx limit, [`IoError::Unsupported`] for a
/// file ufbx reads as something other than FBX, [`IoError::Cancelled`].
pub(crate) fn load(bytes: &[u8], ctx: &ImportCtx<'_>) -> Result<SceneRoot, IoError> {
    let progress = |p: &Progress| match ctx.progress.checkpoint(STAGE, fraction(p.bytes_read, p.bytes_total)) {
        Ok(()) => ProgressResult::Continue,
        Err(_) => ProgressResult::Cancel,
    };
    let allocator = || AllocatorOpts {
        memory_limit: MEMORY_LIMIT_BYTES,
        allocation_limit: ALLOCATION_LIMIT,
        ..AllocatorOpts::default()
    };
    let opts = LoadOpts {
        temp_allocator: allocator(),
        result_allocator: allocator(),
        // FBX only: an OBJ never gets this far (`is_fbx`), and ufbx is told not to guess.
        file_format: FileFormat::Fbx,
        no_format_from_content: true,
        no_format_from_extension: true,
        load_external_files: false,
        ignore_animation: true,
        // Strings are read without a check, so an invalid one must stop the load instead of reaching the scene.
        unicode_error_handling: UnicodeErrorHandling::AbortLoading,
        node_depth_limit: NODE_DEPTH_LIMIT,
        target_axes: TARGET_AXES,
        target_unit_meters: 1.0,
        // The conversion stays in the root node's transform, which `nodes::Basis` applies to every node: `roots` are
        // the root's children, so the root's own transform would otherwise be lost.
        space_conversion: SpaceConversion::TransformRoot,
        geometry_transform_handling: GeometryTransformHandling::ModifyGeometry,
        inherit_mode_handling: InheritModeHandling::Compensate,
        progress_cb: ProgressCb::Ref(&progress),
        ..LoadOpts::default()
    };
    // A progress cancel is reported by ufbx as `Cancelled`; check the handle once more for a cancel that raced the end.
    let scene = ufbx::load_memory(bytes, opts).map_err(|e| map_error(&e))?;
    ctx.progress.checkpoint(STAGE, 1.0)?;
    Ok(scene)
}

/// `done` of `total` as a progress fraction in 0..=1 (0 when there is no total).
#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)] // a progress bar needs no more than f32 digits
pub(crate) fn fraction(done: u64, total: u64) -> f32 {
    if total == 0 { 0.0 } else { (done as f64 / total as f64).clamp(0.0, 1.0) as f32 }
}

/// Maps ufbx's error onto the contract's.
fn map_error(error: &ufbx::Error) -> IoError {
    let what = error.description.to_string();
    let limit = |what: &'static str, limit: usize| IoError::Limit {
        what,
        limit: limit as u64,
        // ufbx stops at the limit and doesn't say how far the file would have gone, so this is "more than the limit".
        got: (limit as u64).saturating_add(1),
    };
    match error.type_ {
        ErrorType::Cancelled => IoError::Cancelled,
        ErrorType::MemoryLimit | ErrorType::OutOfMemory => {
            limit("memory used to read the file in bytes", MEMORY_LIMIT_BYTES)
        }
        ErrorType::AllocationLimit => limit("allocations made to read the file", ALLOCATION_LIMIT),
        ErrorType::NodeDepthLimit => limit("node hierarchy depth", NODE_DEPTH_LIMIT as usize),
        ErrorType::UnrecognizedFileFormat | ErrorType::UnsupportedVersion => {
            IoError::unsupported(what, "convert it to glTF or VRM")
        }
        _ => IoError::Malformed { message: what, at: None },
    }
}
