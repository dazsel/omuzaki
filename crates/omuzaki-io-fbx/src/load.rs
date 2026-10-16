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
/// What ufbx may use on top of the fixed floor for each node the caller allows: a node, its name, its properties and the
/// parsed document behind them. Generous (a node is about a kilobyte), so a real file never reaches it.
const MEMORY_PER_NODE_BYTES: usize = 64 << 10;
/// The memory a file may use whatever its node cap: the document and the settings have a fixed cost.
const MEMORY_FLOOR_BYTES: usize = 16 << 20;
/// Allocations ufbx may make for each node the caller allows, and the floor under them.
const ALLOCATIONS_PER_NODE: usize = 256;
const ALLOCATION_FLOOR: usize = 100_000;
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
    // The caller's node cap sets how much ufbx may build, up to the fixed ceilings: a tight cap gets a tight allocator,
    // so a file cannot make the load spend far more than the caller allowed before the count is checked afterwards.
    let (memory_limit, allocation_limit) = ceilings(ctx.options.limits.max_nodes);
    let allocator = || AllocatorOpts { memory_limit, allocation_limit, ..AllocatorOpts::default() };
    let opts = LoadOpts {
        temp_allocator: allocator(),
        result_allocator: allocator(),
        // FBX only: an OBJ never gets this far (`is_fbx`), and ufbx is told not to guess.
        file_format: FileFormat::Fbx,
        no_format_from_content: true,
        no_format_from_extension: true,
        load_external_files: false,
        // This version reads the node hierarchy only. Vertex arrays and embedded images are the bulk of a real file and
        // are skipped, so their size costs nothing; the meshes, materials, skins, textures and animation stacks that
        // exist are still counted, and `nodes::convert` says they were not read. Animation curves are skipped too; whether
        // the file has any is read from its bytes (`mentions_animation`).
        ignore_geometry: true,
        ignore_embedded: true,
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
    let scene = ufbx::load_memory(bytes, opts).map_err(|e| map_error(&e, memory_limit, allocation_limit))?;
    ctx.progress.checkpoint(STAGE, 1.0)?;
    Ok(scene)
}

/// `done` of `total` as a progress fraction in 0..=1 (0 when there is no total).
#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)] // a progress bar needs no more than f32 digits
pub(crate) fn fraction(done: u64, total: u64) -> f32 {
    if total == 0 { 0.0 } else { (done as f64 / total as f64).clamp(0.0, 1.0) as f32 }
}

/// The object that FBX animation is made of, as an `Objects` child: ASCII writes `AnimationStack: id, ...` and binary a
/// record of that name.
const ANIMATION_OBJECT: &[u8] = b"AnimationStack";
/// The first bytes of a binary FBX: the text `Kaydara FBX Binary  ` and a NUL.
const BINARY_MAGIC: &[u8] = b"Kaydara FBX Binary  \0";
/// Where a binary file's first record starts: the magic, the two bytes `0x1A 0x00`, and the version.
const BINARY_FIRST_RECORD: usize = 27;
/// From this version on, a binary record's offsets and counts are 64 bits wide, not 32.
const WIDE_RECORDS_FROM: u32 = 7500;

/// Whether the file has animation. Curves are not loaded (they can be large and are not imported), so the file's own
/// structure is read for an `AnimationStack` object instead: in ASCII, a line that begins with it; in binary, a child of
/// the top-level `Objects` record. A string that merely contains the name (a model called that) is not animation. One
/// bounded pass over input that `Limits::max_input_bytes` already caps.
pub(crate) fn mentions_animation(bytes: &[u8]) -> bool {
    if bytes.starts_with(BINARY_MAGIC) { binary_has_animation(bytes) } else { ascii_has_animation(bytes) }
}

fn ascii_has_animation(bytes: &[u8]) -> bool {
    let mut object = ANIMATION_OBJECT.to_vec();
    object.push(b':');
    bytes.split(|&b| b == b'\n').any(|line| line.trim_ascii_start().starts_with(&object))
}

/// One binary record's header: where it ends, how long its properties are, its name, and where its body starts.
struct Record<'a> {
    end: usize,
    props_len: usize,
    name: &'a [u8],
    body: usize,
}

fn record(bytes: &[u8], at: usize, wide: bool) -> Option<Record<'_>> {
    let word = |offset: usize, width: usize| -> Option<usize> {
        let raw = bytes.get(at.checked_add(offset)?..at.checked_add(offset)?.checked_add(width)?)?;
        let mut padded = [0_u8; 8];
        padded[..width].copy_from_slice(raw);
        usize::try_from(u64::from_le_bytes(padded)).ok()
    };
    let width = if wide { 8 } else { 4 };
    let (end, props_len) = (word(0, width)?, word(2 * width, width)?);
    let fixed = 3 * width;
    let name_len = usize::from(*bytes.get(at.checked_add(fixed)?)?);
    let start = at.checked_add(fixed)?.checked_add(1)?;
    let name = bytes.get(start..start.checked_add(name_len)?)?;
    Some(Record { end, props_len, name, body: start + name_len })
}

fn binary_has_animation(bytes: &[u8]) -> bool {
    let Some(version) = bytes.get(23..27).and_then(|v| v.try_into().ok()).map(u32::from_le_bytes) else {
        return false;
    };
    let wide = version >= WIDE_RECORDS_FROM;
    let mut at = BINARY_FIRST_RECORD;
    // Each step moves forward to a record end inside the file, so the walk is as long as the file and no longer.
    while let Some(top) = record(bytes, at, wide).filter(|r| r.end > at && r.end <= bytes.len()) {
        if top.name == b"Objects" {
            let mut child = top.body.saturating_add(top.props_len);
            while let Some(object) = record(bytes, child, wide).filter(|r| r.end > child && r.end <= top.end) {
                if object.name == ANIMATION_OBJECT {
                    return true;
                }
                child = object.end;
            }
        }
        at = top.end;
    }
    false
}

/// The memory (bytes) and allocation ceilings a load runs under for a node cap: the floor plus a share for each node the
/// caller allows, never over the fixed ceilings.
pub(crate) fn ceilings(max_nodes: u32) -> (usize, usize) {
    let nodes = usize::try_from(max_nodes).unwrap_or(usize::MAX);
    (
        MEMORY_FLOOR_BYTES.saturating_add(nodes.saturating_mul(MEMORY_PER_NODE_BYTES)).min(MEMORY_LIMIT_BYTES),
        ALLOCATION_FLOOR.saturating_add(nodes.saturating_mul(ALLOCATIONS_PER_NODE)).min(ALLOCATION_LIMIT),
    )
}

/// Maps ufbx's error onto the contract's. The limits are the ceilings this load ran under (see [`ceilings`]), which the
/// error names, not the fixed maxima.
fn map_error(error: &ufbx::Error, memory_limit: usize, allocation_limit: usize) -> IoError {
    let what = error.description.to_string();
    let limit = |what: &'static str, limit: usize| IoError::Limit {
        what,
        limit: limit as u64,
        // ufbx stops at the limit and doesn't say how far the file would have gone, so this is "more than the limit".
        got: (limit as u64).saturating_add(1),
    };
    match error.type_ {
        ErrorType::Cancelled => IoError::Cancelled,
        ErrorType::MemoryLimit | ErrorType::OutOfMemory => limit("memory used to read the file in bytes", memory_limit),
        ErrorType::AllocationLimit => limit("allocations made to read the file", allocation_limit),
        ErrorType::NodeDepthLimit => limit("node hierarchy depth", NODE_DEPTH_LIMIT as usize),
        ErrorType::UnrecognizedFileFormat | ErrorType::UnsupportedVersion => {
            IoError::unsupported(what, "convert it to glTF or VRM")
        }
        _ => IoError::Malformed { message: what, at: None },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_tight_node_cap_gets_a_tight_allocator_and_a_loose_one_the_fixed_ceilings() {
        let (memory, allocations) = ceilings(2);
        assert!(memory < 17 << 20 && allocations < 200_000, "{memory} {allocations}");
        // The default cap of 100,000 nodes reaches the fixed memory ceiling; its allocation share is under the fixed one.
        assert_eq!(ceilings(100_000), (MEMORY_LIMIT_BYTES, 25_700_000));
        assert_eq!(ceilings(u32::MAX), (MEMORY_LIMIT_BYTES, ALLOCATION_LIMIT));
    }
}
