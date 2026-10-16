//! The per-section cache behind the partial repaint (decision 7.2 of the research note, #1670): one encoded
//! `vello::Scene` per section of every layer, keyed by a fingerprint of the section's slice of the draw list, so a frame
//! re-encodes only the sections whose picture changed.

use std::ops::Range;

use omuzaki_face_api::{Cap, Cmd, DrawList, FaceError, FaceLayerSpec, Join, Paint, PathEl, Stops};
use vello::Scene;
use vello::kurbo::Rect;

use crate::painter::LayerSections;

/// One section's encoded scene, the key it was encoded under and the pixel rectangle it covers.
#[derive(Default)]
pub(super) struct SectionCache {
    pub(super) scene: Scene,
    /// The fingerprint of the slice the scene encodes; `None` until the section has been encoded.
    pub(super) key: Option<u64>,
    /// What the scene drew into, or `None` if it drew nothing.
    pub(super) bounds: Option<Rect>,
}

/// The scenes of one layer's sections.
pub(super) type LayerCache = [SectionCache; crate::painter::Section::ALL.len()];

/// The fingerprint of `list`'s commands in `range` under `spec`: FNV-1a over the exact bit patterns of every command, the
/// path elements it refers to and the layer's rectangle and density, the way `DrawList::fingerprint` hashes a whole list.
/// Equal fingerprints encode to equal scenes.
///
/// # Errors
/// [`FaceError::Raster`] if `range` is not inside the list.
pub(super) fn fingerprint(list: &DrawList, range: Range<usize>, spec: &FaceLayerSpec) -> Result<u64, FaceError> {
    let cmds = list.cmds().get(range.clone()).ok_or_else(|| {
        FaceError::Raster(format!("the section range {range:?} is outside the {} commands", list.cmds().len()))
    })?;
    let mut hash = Fnv::new();
    for value in [spec.rect.x0, spec.rect.x1, spec.rect.y0, spec.rect.y1, spec.px_per_m] {
        hash.f(value);
    }
    for cmd in cmds {
        hash.cmd(cmd, list);
    }
    Ok(hash.0)
}

/// Checks the ranges tile the layer's list in order, so no command is encoded twice or left out.
///
/// # Errors
/// [`FaceError::Raster`] naming the first range that doesn't continue the previous one, or the list's end.
pub(super) fn check_tiling(sections: &LayerSections, len: usize) -> Result<(), FaceError> {
    let mut end = 0;
    for (at, range) in sections.iter().enumerate() {
        if range.start != end || range.end < range.start {
            return Err(FaceError::Raster(format!("section {at}'s range {range:?} doesn't continue at {end}")));
        }
        end = range.end;
    }
    if end == len {
        Ok(())
    } else {
        Err(FaceError::Raster(format!("the section ranges end at {end}, but the list has {len} commands")))
    }
}

struct Fnv(u64);

impl Fnv {
    fn new() -> Self {
        Self(0xcbf2_9ce4_8422_2325)
    }

    fn byte(&mut self, b: u8) {
        self.0 = (self.0 ^ u64::from(b)).wrapping_mul(0x0100_0000_01b3);
    }

    fn f(&mut self, v: f32) {
        for b in v.to_bits().to_le_bytes() {
            self.byte(b);
        }
    }

    fn v2(&mut self, v: omuzaki_core::glam::Vec2) {
        self.f(v.x);
        self.f(v.y);
    }

    fn stops(&mut self, stops: &Stops) {
        for stop in stops.as_slice() {
            self.f(stop.offset);
            for b in [stop.colour.r, stop.colour.g, stop.colour.b, stop.colour.a] {
                self.byte(b);
            }
        }
    }

    fn paint(&mut self, paint: &Paint) {
        match paint {
            Paint::Solid(c) => {
                self.byte(1);
                for b in [c.r, c.g, c.b, c.a] {
                    self.byte(b);
                }
            }
            Paint::Linear { from, to, stops } => {
                self.byte(2);
                self.v2(*from);
                self.v2(*to);
                self.stops(stops);
            }
            Paint::Radial { centre, radius, stops } => {
                self.byte(3);
                self.v2(*centre);
                self.f(*radius);
                self.stops(stops);
            }
        }
    }

    fn path(&mut self, list: &DrawList, path: omuzaki_face_api::PathRef) {
        for el in list.elements(path) {
            match *el {
                PathEl::Move(a) => {
                    self.byte(10);
                    self.v2(a);
                }
                PathEl::Line(a) => {
                    self.byte(11);
                    self.v2(a);
                }
                PathEl::Quad(a, b) => {
                    self.byte(12);
                    self.v2(a);
                    self.v2(b);
                }
                PathEl::Cubic(a, b, c) => {
                    self.byte(13);
                    self.v2(a);
                    self.v2(b);
                    self.v2(c);
                }
                PathEl::Close => self.byte(14),
            }
        }
        self.byte(15);
    }

    fn cmd(&mut self, cmd: &Cmd, list: &DrawList) {
        match cmd {
            Cmd::Fill { path, paint, opacity } => {
                self.byte(20);
                self.path(list, *path);
                self.paint(paint);
                self.f(*opacity);
            }
            Cmd::Stroke { path, paint, width, cap, join, opacity } => {
                self.byte(21);
                self.path(list, *path);
                self.paint(paint);
                self.f(*width);
                self.byte(u8::from(*cap == Cap::Round));
                self.byte(u8::from(*join == Join::Round));
                self.f(*opacity);
            }
            Cmd::PushClip(path) => {
                self.byte(22);
                self.path(list, *path);
            }
            Cmd::PopClip => self.byte(23),
            Cmd::PushTransform(t) => {
                self.byte(24);
                for v in t.to_cols_array() {
                    self.f(v);
                }
            }
            Cmd::PopTransform => self.byte(25),
        }
    }
}
