//! [`CpuLayer`]: one layer's CPU raster, repainted in part when only some of its commands change (#2443).
//!
//! A tracked face changes every frame, but only some of its commands do: the eyes and the mouth move, while the warm
//! wash under them (a radial gradient over the whole layer, most of a full CPU repaint) stays put. So the layer keeps:
//!
//! - a **snapshot** of what the first `base` commands draw, taken where no clip or transform is open, so drawing on
//!   from it is the same as drawing from the start;
//! - each command's **key** (its fingerprint with the transform it is drawn under) and the pixel rectangle it can touch.
//!
//! A repaint compares the new keys with the last ones. The **damage** is every changed command's rectangle, before and
//! after. If nothing before `base` changed, the damage is copied from the snapshot and the commands from `base` on are
//! drawn over it, inside the damage only: every pixel comes out as a whole repaint would draw it, because every
//! command that can reach it is drawn again, in order, from the same starting pixels. Otherwise the layer is drawn
//! whole and a new snapshot is taken before the first command that changed.
//!
//! `base` follows the earliest change of the last [`WINDOW`] repaints, so a command that changes now and then (a blush
//! that a smile raises) moves it back once, and it moves forward again (drawing the commands it passes into the
//! snapshot) once those commands have held still for a while.

use omuzaki_core::glam::Affine2;
use omuzaki_face_api::{Cmd, DrawList, FaceError, FaceLayerSpec, FaceRasterizer, Raster, RasterRect};

use super::bounds::{command_extent, to_pixels};

/// How many repaints `base` looks back over before it moves forward.
pub const WINDOW: u32 = 8;

/// What one [`CpuLayer::repaint`] did.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum Repaint {
    /// Nothing changed.
    Unchanged,
    /// The whole layer was drawn.
    Whole,
    /// Only this rectangle was drawn again.
    Part(RasterRect),
}

/// One layer's CPU raster and what it needs to repaint it in part. Reuse it every frame: once warm, a repaint
/// allocates nothing.
#[derive(Clone, Debug)]
pub struct CpuLayer {
    raster: Raster,
    snapshot: Raster,
    /// The commands the snapshot holds: `..base` of the last list.
    base: usize,
    /// The spec the raster was drawn for; `None` until the first repaint, and after a failed one.
    spec: Option<FaceLayerSpec>,
    keys: Vec<u64>,
    bounds: Vec<Option<RasterRect>>,
    /// Scratch for the new list's keys, bounds and safe points.
    next_keys: Vec<u64>,
    next_bounds: Vec<Option<RasterRect>>,
    safe: Vec<bool>,
    transforms: Vec<Affine2>,
    clips: Vec<(Option<RasterRect>, u64)>,
    /// The common-subsequence table of the last comparison.
    lcs: Vec<u16>,
    /// The rectangles to draw again, merged where they overlap or nearly do.
    damage: Vec<RasterRect>,
    /// The earliest change seen over the current window, and how many repaints the window holds so far.
    earliest: usize,
    seen: u32,
}

impl Default for CpuLayer {
    fn default() -> Self {
        Self {
            raster: Raster::new(0, 0),
            snapshot: Raster::new(0, 0),
            base: 0,
            spec: None,
            keys: Vec::new(),
            bounds: Vec::new(),
            next_keys: Vec::new(),
            next_bounds: Vec::new(),
            safe: Vec::new(),
            transforms: Vec::new(),
            clips: Vec::new(),
            lcs: Vec::new(),
            damage: Vec::new(),
            earliest: usize::MAX,
            seen: 0,
        }
    }
}

impl CpuLayer {
    /// The raster as of the last successful [`repaint`](Self::repaint).
    pub fn raster(&self) -> &Raster {
        &self.raster
    }

    /// The rectangles the last partial repaint drew again.
    pub fn damage(&self) -> &[RasterRect] {
        &self.damage
    }

    /// How many commands the snapshot holds.
    pub fn base(&self) -> usize {
        self.base
    }

    /// Forgets everything, so the next repaint draws the whole layer.
    pub fn invalidate(&mut self) {
        self.spec = None;
    }

    /// Brings the raster up to date with `list` drawn for `spec`, drawing only what changed when `cpu` can draw part of
    /// a list ([`FaceRasterizer::rasterize_part`]).
    ///
    /// # Errors
    /// What the rasteriser returns. The layer is then drawn whole on the next repaint.
    pub fn repaint(
        &mut self,
        cpu: &mut dyn FaceRasterizer,
        spec: &FaceLayerSpec,
        list: &DrawList,
    ) -> Result<Repaint, FaceError> {
        let result = self.try_repaint(cpu, spec, list);
        if result.is_err() {
            self.spec = None;
        }
        result
    }

    fn try_repaint(
        &mut self,
        cpu: &mut dyn FaceRasterizer,
        spec: &FaceLayerSpec,
        list: &DrawList,
    ) -> Result<Repaint, FaceError> {
        let (width, height) = spec.size_px();
        let scratch = Scratch {
            keys: &mut self.next_keys,
            bounds: &mut self.next_bounds,
            safe: &mut self.safe,
            transforms: &mut self.transforms,
            clips: &mut self.clips,
        };
        describe(list, spec, (width, height), scratch);
        let len = list.cmds().len();
        if self.spec != Some(*spec) || (self.raster.width, self.raster.height) != (width, height) {
            // A new layer, or a new size: draw it whole and snapshot nothing until a change shows where to.
            self.whole(cpu, spec, list, 0)?;
            self.earliest = usize::MAX;
            self.seen = 0;
            return Ok(Repaint::Whole);
        }
        let Some(first) = self.changes(width, height) else {
            self.swap_keys();
            return Ok(Repaint::Unchanged);
        };
        self.earliest = self.earliest.min(first);
        self.seen += 1;
        if first < self.base {
            // Something the snapshot holds changed: draw it whole, snapshotting before the first change.
            let base = self.safe_at_or_before(first);
            self.whole(cpu, spec, list, base)?;
            self.earliest = first;
            self.seen = 1;
            return Ok(Repaint::Whole);
        }
        if self.seen >= WINDOW {
            // The commands up to the window's earliest change have held still: draw them into the snapshot once, so
            // later repaints start after them.
            let target = self.safe_at_or_before(self.earliest.min(len));
            if target > self.base
                && !cpu.rasterize_part(
                    spec,
                    list,
                    spec.px_per_m,
                    self.base..target,
                    RasterRect::full(width, height),
                    &mut self.snapshot,
                )?
            {
                self.whole(cpu, spec, list, 0)?;
                return Ok(Repaint::Whole);
            }
            self.base = self.base.max(target);
            self.earliest = usize::MAX;
            self.seen = 0;
        }
        let Some(union) = self.damage.iter().copied().reduce(RasterRect::union) else {
            self.swap_keys();
            return Ok(Repaint::Unchanged);
        };
        for &rect in &self.damage {
            copy_rect(&self.snapshot, &mut self.raster, rect);
            if !cpu.rasterize_part(spec, list, spec.px_per_m, self.base..len, rect, &mut self.raster)? {
                self.whole(cpu, spec, list, 0)?;
                return Ok(Repaint::Whole);
            }
        }
        self.swap_keys();
        Ok(Repaint::Part(union))
    }

    /// The first command whose key changed, if one did, with [`damage`](Self::damage) set to the rectangles the changed
    /// commands covered or cover now.
    ///
    /// The lists are aligned on their keys (a longest common subsequence), not compared index by index: a command that
    /// appears or goes away (a blush patch that fades out) shifts every later index, and the commands after it have not
    /// changed. A command that kept its key, its transform and its clips draws the same pixels, and the order of the
    /// kept commands is the same in both lists, so outside the changed commands' rectangles every pixel is as before.
    fn changes(&mut self, width: u32, height: u32) -> Option<usize> {
        let (old, new) = (&self.keys, &self.next_keys);
        let (old_len, new_len) = (old.len(), new.len());
        let prefix = old.iter().zip(new).take_while(|(a, b)| a == b).count();
        if prefix == old_len && old_len == new_len {
            self.damage.clear();
            return None;
        }
        let suffix = old[prefix..].iter().rev().zip(new[prefix..].iter().rev()).take_while(|(a, b)| a == b).count();
        let (gone, came) = (&old[prefix..old_len - suffix], &new[prefix..new_len - suffix]);
        // The table of common-subsequence lengths of the suffixes of `gone` and `came`.
        let (rows, cols) = (gone.len() + 1, came.len() + 1);
        self.lcs.clear();
        self.lcs.resize(rows * cols, 0);
        for i in (0..gone.len()).rev() {
            for j in (0..came.len()).rev() {
                self.lcs[i * cols + j] = if gone[i] == came[j] {
                    self.lcs[(i + 1) * cols + j + 1] + 1
                } else {
                    self.lcs[(i + 1) * cols + j].max(self.lcs[i * cols + j + 1])
                };
            }
        }
        self.damage.clear();
        let changed = |rect: Option<&Option<RasterRect>>, damage: &mut Vec<RasterRect>| {
            if let Some(Some(rect)) = rect
                && rect.x < width
                && rect.y < height
                && !rect.is_empty()
            {
                add_rect(damage, *rect);
            }
        };
        let (mut i, mut j) = (0, 0);
        while i < gone.len() || j < came.len() {
            if i < gone.len() && j < came.len() && gone[i] == came[j] {
                (i, j) = (i + 1, j + 1);
            } else if j < came.len() && (i == gone.len() || self.lcs[i * cols + j + 1] >= self.lcs[(i + 1) * cols + j])
            {
                changed(self.next_bounds.get(prefix + j), &mut self.damage);
                j += 1;
            } else {
                changed(self.bounds.get(prefix + i), &mut self.damage);
                i += 1;
            }
        }
        Some(prefix)
    }

    /// The last safe point (no clip or transform open) at or before `at`.
    fn safe_at_or_before(&self, at: usize) -> usize {
        (0..=at.min(self.safe.len().saturating_sub(1))).rev().find(|&i| self.safe[i]).unwrap_or(0)
    }

    /// Draws the whole layer, snapshotting the first `base` commands.
    fn whole(
        &mut self,
        cpu: &mut dyn FaceRasterizer,
        spec: &FaceLayerSpec,
        list: &DrawList,
        base: usize,
    ) -> Result<(), FaceError> {
        let (width, height) = spec.size_px();
        let len = list.cmds().len();
        let full = RasterRect::full(width, height);
        clear(&mut self.snapshot, width, height);
        let parted = cpu.rasterize_part(spec, list, spec.px_per_m, 0..base, full, &mut self.snapshot)?;
        if parted {
            self.raster.width = width;
            self.raster.height = height;
            self.raster.pixels.clone_from(&self.snapshot.pixels);
            if !cpu.rasterize_part(spec, list, spec.px_per_m, base..len, full, &mut self.raster)? {
                return Err(FaceError::Raster("the rasteriser stopped drawing parts of a list".to_owned()));
            }
            self.base = base;
        } else {
            cpu.rasterize(spec, list, spec.px_per_m, &mut self.raster)?;
            self.base = 0;
        }
        self.spec = Some(*spec);
        self.swap_keys();
        Ok(())
    }

    fn swap_keys(&mut self) {
        std::mem::swap(&mut self.keys, &mut self.next_keys);
        std::mem::swap(&mut self.bounds, &mut self.next_bounds);
    }
}

/// At most this many separate rectangles are drawn again; more are merged.
const MAX_RECTS: usize = 6;

/// Adds `rect` to `rects`, merging rectangles that overlap or would cost little more drawn as one: drawing a part costs
/// a pass over the commands that reach it, so two nearby pieces are cheaper as their union.
fn add_rect(rects: &mut Vec<RasterRect>, rect: RasterRect) {
    let mut rect = rect;
    // Merge with any rectangle it overlaps or nearly fills a union with, then repeat for the grown rectangle.
    while let Some(at) = rects.iter().position(|r| {
        let union = r.union(rect);
        r.intersects(&rect) || union.area() * 4 <= (r.area() + rect.area()) * 5
    }) {
        rect = rect.union(rects.swap_remove(at));
    }
    rects.push(rect);
    while rects.len() > MAX_RECTS {
        // Merge the pair whose union adds the least.
        let mut best = (0, 1, u64::MAX);
        for i in 0..rects.len() {
            for j in i + 1..rects.len() {
                let cost = rects[i].union(rects[j]).area().saturating_sub(rects[i].area() + rects[j].area());
                if cost < best.2 {
                    best = (i, j, cost);
                }
            }
        }
        let merged = rects.swap_remove(best.1);
        let other = rects.swap_remove(best.0);
        add_rect(rects, merged.union(other));
    }
}

/// FNV-style mixing of `value` into `hash`.
fn mix(hash: u64, value: u64) -> u64 {
    (hash.rotate_left(5) ^ value).wrapping_mul(0x0100_0000_01b3)
}

/// Resizes `raster` to `width` x `height` and makes it transparent.
fn clear(raster: &mut Raster, width: u32, height: u32) {
    raster.width = width;
    raster.height = height;
    raster.pixels.clear();
    raster.pixels.resize(width as usize * height as usize * 4, 0);
}

/// Copies `rect` of `from` into `to` (the same size).
fn copy_rect(from: &Raster, to: &mut Raster, rect: RasterRect) {
    let stride = from.width as usize * 4;
    let (x0, x1) = (rect.x as usize * 4, (rect.x + rect.width) as usize * 4);
    for y in rect.y as usize..(rect.y + rect.height) as usize {
        let row = y * stride;
        to.pixels[row + x0..row + x1].copy_from_slice(&from.pixels[row + x0..row + x1]);
    }
}

/// Where [`describe`] writes.
struct Scratch<'a> {
    keys: &'a mut Vec<u64>,
    bounds: &'a mut Vec<Option<RasterRect>>,
    safe: &'a mut Vec<bool>,
    transforms: &'a mut Vec<Affine2>,
    /// What each open clip leaves (`None`: nothing), and the clip context outside it.
    clips: &'a mut Vec<(Option<RasterRect>, u64)>,
}

/// Fills `keys` and `bounds` with each command's key (its fingerprint and the transform it is drawn under) and the
/// pixel rectangle it can touch inside a `size` raster, and `safe` with whether no clip or transform is open before
/// each command (one more entry than commands: the end).
fn describe(list: &DrawList, spec: &FaceLayerSpec, size: (u32, u32), out: Scratch<'_>) {
    let Scratch { keys, bounds, safe, transforms, clips } = out;
    keys.clear();
    bounds.clear();
    safe.clear();
    transforms.clear();
    let to_px = to_pixels(spec, size);
    let mut transform = Affine2::IDENTITY;
    let mut context = 0_u64;
    // What the open clips leave, as one rectangle (`None`: nothing). Drawing under a clip reaches only inside it.
    clips.clear();
    for (index, cmd) in list.cmds().iter().enumerate() {
        safe.push(transforms.is_empty() && clips.is_empty());
        // The command, the transform it is drawn under and the clips it is drawn inside: the same key draws the same
        // pixels.
        let mut key = list.cmd_fingerprint(index).unwrap_or(0) ^ context.rotate_left(17);
        for v in transform.to_cols_array() {
            key = mix(key, u64::from(v.to_bits()));
        }
        keys.push(key);
        let total = to_px * transform;
        let inside = |rect: Option<RasterRect>| match clips.last() {
            Some((clip, _)) => rect.zip(*clip).and_then(|(r, c)| r.intersection(c)),
            None => rect,
        };
        let rect = match *cmd {
            Cmd::Fill { .. } | Cmd::Stroke { .. } | Cmd::PushClip(_) => inside(command_extent(cmd, list, total, size)),
            Cmd::PushTransform(t) => {
                transforms.push(transform);
                transform *= t;
                None
            }
            Cmd::PopTransform => {
                transform = transforms.pop().unwrap_or(Affine2::IDENTITY);
                None
            }
            Cmd::PopClip => {
                if let Some((_, outer)) = clips.pop() {
                    context = outer;
                }
                None
            }
        };
        if matches!(cmd, Cmd::PushClip(_)) {
            clips.push((rect, context));
            context = mix(context, key);
        }
        bounds.push(rect);
    }
    safe.push(transforms.is_empty() && clips.is_empty());
}
