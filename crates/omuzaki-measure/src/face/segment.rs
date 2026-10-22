//! The face region (design D4 and 4.1): the lit skin, bounded by the line art, with every enclosed mark filled in.
//!
//! In order: the lit skin colour from three patches below and between the eyes; the line's lightness from rays down the
//! chin sector, which sets how dark a pixel may be and still count as skin; the skin-like pixels; a flood fill from the
//! patches; and the holes (an eye, the mouth, a blush) joined to the region. The background's colour, when the ROI's
//! border has one, is found here too, for the outline's `beyond`.

use std::collections::{BTreeMap, VecDeque};

use super::frame::{FaceFrame, Roi};

/// The patch centres, face coordinates (IOD): below each eye and between them.
pub const PATCHES: [[f64; 2]; 3] = [[-0.5, 0.42], [0.5, 0.42], [0.0, 0.22]];

/// The patch radius, IOD.
pub const PATCH_RADIUS: f64 = 0.07;

/// How far above the lit skin a pixel may be and still count as skin, `L*`.
pub const SKIN_ABOVE: f64 = 10.0;

/// The deepest the skin window reaches below the lit skin, `L*`.
pub const SKIN_BELOW_MAX: f64 = 20.0;

/// The largest chroma difference (in `a*b*`) a skin-like pixel may have from the lit skin.
pub const SKIN_CHROMA: f64 = 14.0;

/// A line must be at least this much darker than the lit skin to be found as the line (`L*`).
pub const LINE_FIND: f64 = 10.0;

/// Below this contrast between the lit skin and the line the line art cannot bound the face (`L*`).
pub const MIN_CONTRAST: f64 = 15.0;

/// The ray step, ROI pixels.
pub const STEP: f64 = 0.25;

/// The rays start this far from the eye midpoint, IOD: outside the eyes.
pub const RAY_START: f64 = 0.30;

/// A colour within this CIE76 distance of the border's mode is background.
pub const BACKGROUND_DELTA_E: f64 = 6.0;

/// What the segmentation found.
#[derive(Clone, Debug)]
pub struct Segmentation {
    /// The lit skin, CIELAB.
    pub skin: [f64; 3],
    /// The line art's lightness, when rays down the chin found one.
    pub line_l: Option<f64>,
    /// The skin window's lower bound, relative to the lit skin (a negative `L*` difference).
    pub lower_dl: f64,
    /// Each ROI pixel: skin-like by colour.
    pub skin_like: Vec<bool>,
    /// Each ROI pixel: inside the face region (the fill plus its holes).
    pub filled: Vec<bool>,
    /// The background colour, CIELAB, when at least half of the ROI's border has it.
    pub background: Option<[f64; 3]>,
}

impl Segmentation {
    /// Whether the colour `lab` is skin-like (the alpha test is the caller's).
    #[must_use]
    pub fn is_skin_colour(&self, lab: [f64; 3]) -> bool {
        let dl = lab[0] - self.skin[0];
        dl > self.lower_dl && dl < SKIN_ABOVE && (lab[1] - self.skin[1]).hypot(lab[2] - self.skin[2]) < SKIN_CHROMA
    }

    /// The mask of the snap: pixels inside the face region that are not skin (the marks).
    #[must_use]
    pub fn mark(&self, index: usize) -> bool {
        self.filled[index] && !self.skin_like[index]
    }
}

/// Why the face region could not be found.
#[derive(Clone, Debug, PartialEq)]
pub enum SegmentError {
    /// No opaque pixel lies in the patches below the eyes, so the skin colour is unknown.
    NoSkin,
    /// The line art is too faint: its contrast with the lit skin, `L*`.
    LowContrast(f64),
}

/// The pixels whose centres lie within `radius` ROI pixels of `centre`, in index order.
pub(crate) fn disc(roi: &Roi, centre: [f64; 2], radius: f64) -> Vec<usize> {
    let r2 = radius * radius;
    let (x0, x1) = ((centre[0] - radius).floor().max(0.0), (centre[0] + radius).ceil());
    let (y0, y1) = ((centre[1] - radius).floor().max(0.0), (centre[1] + radius).ceil());
    let mut out = Vec::new();
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // clamped at 0; capped by the ROI below
    let (x0, x1, y0, y1) = (x0 as usize, x1 as usize, y0 as usize, y1 as usize);
    for y in y0..=y1.min(roi.height.saturating_sub(1)) {
        for x in x0..=x1.min(roi.width.saturating_sub(1)) {
            #[allow(clippy::cast_precision_loss)] // ROI sides are small
            let (cx, cy) = (x as f64 + 0.5, y as f64 + 0.5);
            if (cx - centre[0]).powi(2) + (cy - centre[1]).powi(2) <= r2 {
                out.push(y * roi.width + x);
            }
        }
    }
    out
}

/// The component-wise median of `values` (the lower middle of an even count).
fn median3(values: &mut [[f64; 3]]) -> Option<[f64; 3]> {
    if values.is_empty() {
        return None;
    }
    let mut out = [0.0; 3];
    for (c, slot) in out.iter_mut().enumerate() {
        values.sort_by(|a, b| a[c].total_cmp(&b[c]));
        *slot = values[(values.len() - 1) / 2][c];
    }
    Some(out)
}

/// The median of `values` (the lower middle of an even count).
pub(crate) fn median(values: &mut [f64]) -> Option<f64> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(f64::total_cmp);
    Some(values[(values.len() - 1) / 2])
}

/// The lit skin: the CIELAB median of the lighter half of the opaque pixels in the three patches.
fn lit_skin(roi: &Roi, frame: &FaceFrame) -> Option<[f64; 3]> {
    let mut pixels: Vec<[f64; 3]> = Vec::new();
    for [x, y] in PATCHES {
        for i in disc(roi, frame.to_px(x, y), PATCH_RADIUS * frame.iod_px) {
            if roi.opaque(i) {
                pixels.push(roi.lab_at(i));
            }
        }
    }
    pixels.sort_by(|a, b| a[0].total_cmp(&b[0]));
    let lighter = pixels.len() / 2;
    median3(&mut pixels[lighter..])
}

/// The line's lightness: along nine rays from 70° to 110°, the first local minimum darker than the lit skin by
/// [`LINE_FIND`]; the median over the rays that found one.
fn line_lightness(roi: &Roi, frame: &FaceFrame, skin_l: f64) -> Option<f64> {
    let mut minima = Vec::new();
    for ray in 0..9_u32 {
        let phi = 70.0 + 5.0 * f64::from(ray);
        let mut t = RAY_START * frame.iod_px;
        let mut found: Option<f64> = None;
        while let Some(l) = roi.lightness(frame.along(phi, t)) {
            match found {
                None if l < skin_l - LINE_FIND => found = Some(l),
                Some(lowest) if l <= lowest => found = Some(l),
                Some(_) => break,
                None => {}
            }
            t += STEP;
        }
        if let Some(l) = found {
            minima.push(l);
        }
    }
    median(&mut minima)
}

/// The colour that the most of the ROI's border has, when at least half of the border is within
/// [`BACKGROUND_DELTA_E`] of it. Colours are binned by 4 in each CIELAB axis; the fullest bin's mean is the mode.
fn background(roi: &Roi) -> Option<[f64; 3]> {
    let (w, h) = (roi.width, roi.height);
    if w == 0 || h == 0 {
        return None;
    }
    let mut border: Vec<usize> = (0..w).chain((0..w).map(|x| (h - 1) * w + x)).collect();
    border.extend((1..h.saturating_sub(1)).flat_map(|y| [y * w, y * w + w - 1]));
    border.sort_unstable();
    border.dedup();
    let mut bins: BTreeMap<[i64; 3], (usize, [f64; 3])> = BTreeMap::new();
    for &i in &border {
        let lab = roi.lab_at(i);
        #[allow(clippy::cast_possible_truncation)] // CIELAB values are within ±200
        let key = lab.map(|v| (v / 4.0).floor() as i64);
        let entry = bins.entry(key).or_insert((0, [0.0; 3]));
        entry.0 += 1;
        for (slot, value) in entry.1.iter_mut().zip(lab) {
            *slot += value;
        }
    }
    let (count, sum) = bins.values().fold((0, [0.0; 3]), |best, &(n, s)| if n > best.0 { (n, s) } else { best });
    #[allow(clippy::cast_precision_loss)] // pixel counts
    let mode = sum.map(|v| v / count as f64);
    let near = border.iter().filter(|&&i| delta_e(roi.lab_at(i), mode) <= BACKGROUND_DELTA_E).count();
    (2 * near >= border.len()).then_some(mode)
}

/// The CIE76 colour difference.
#[must_use]
pub fn delta_e(a: [f64; 3], b: [f64; 3]) -> f64 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}

/// Finds the face region in `roi` around `frame` (both in ROI pixels). `skin` overrides the lit skin colour.
///
/// # Errors
/// [`SegmentError::NoSkin`] when the patches hold no opaque pixel; [`SegmentError::LowContrast`] when the line art is
/// less than [`MIN_CONTRAST`] darker than the lit skin.
pub fn segment(roi: &Roi, frame: &FaceFrame, skin: Option<[f64; 3]>) -> Result<Segmentation, SegmentError> {
    let skin = match skin {
        Some(given) => given,
        None => lit_skin(roi, frame).ok_or(SegmentError::NoSkin)?,
    };
    // No line at all down the chin is the faintest line art: nothing bounds the face (a skin-coloured shape with no
    // drawing on it).
    let line_l = line_lightness(roi, frame, skin[0]).ok_or(SegmentError::LowContrast(0.0))?;
    if skin[0] - line_l < MIN_CONTRAST {
        return Err(SegmentError::LowContrast(skin[0] - line_l));
    }
    let line_l = Some(line_l);
    let lower_dl = -line_l.map_or(SKIN_BELOW_MAX, |line| SKIN_BELOW_MAX.min((skin[0] - line) / 2.0));
    let mut seg = Segmentation {
        skin,
        line_l,
        lower_dl,
        skin_like: Vec::new(),
        filled: vec![false; roi.width * roi.height],
        background: background(roi),
    };
    seg.skin_like = (0..roi.width * roi.height).map(|i| roi.opaque(i) && seg.is_skin_colour(roi.lab_at(i))).collect();
    fill(roi, frame, &mut seg);
    fill_holes(roi, &mut seg.filled);
    Ok(seg)
}

/// The 4-connected flood fill of skin-like pixels from the three patch centres. A centre that is not skin-like seeds from
/// the skin-like pixel of its patch nearest to it instead.
fn fill(roi: &Roi, frame: &FaceFrame, seg: &mut Segmentation) {
    let mut queue = VecDeque::new();
    for [x, y] in PATCHES {
        let centre = frame.to_px(x, y);
        let seed = roi.index(centre).filter(|&i| seg.skin_like[i]).or_else(|| {
            disc(roi, centre, PATCH_RADIUS * frame.iod_px).into_iter().filter(|&i| seg.skin_like[i]).min_by(|&a, &b| {
                let (pa, pb) = (roi.centre(a), roi.centre(b));
                let da = (pa[0] - centre[0]).hypot(pa[1] - centre[1]);
                let db = (pb[0] - centre[0]).hypot(pb[1] - centre[1]);
                da.total_cmp(&db).then(a.cmp(&b))
            })
        });
        if let Some(i) = seed
            && !seg.filled[i]
        {
            seg.filled[i] = true;
            queue.push_back(i);
        }
    }
    let w = roi.width;
    while let Some(i) = queue.pop_front() {
        for next in neighbours(i, w, roi.height) {
            if seg.skin_like[next] && !seg.filled[next] {
                seg.filled[next] = true;
                queue.push_back(next);
            }
        }
    }
}

/// The 4-connected neighbours of pixel `index` in a `width × height` grid.
pub(crate) fn neighbours(index: usize, width: usize, height: usize) -> impl Iterator<Item = usize> {
    let (col, row) = (index % width, index / width);
    [
        (col > 0).then(|| index - 1),
        (col + 1 < width).then(|| index + 1),
        (row > 0).then(|| index - width),
        (row + 1 < height).then(|| index + width),
    ]
    .into_iter()
    .flatten()
}

/// Joins every pixel that the ROI's border cannot reach through non-region pixels to the region.
fn fill_holes(roi: &Roi, filled: &mut [bool]) {
    let (w, h) = (roi.width, roi.height);
    let mut outside = vec![false; w * h];
    let mut queue = VecDeque::new();
    for i in 0..w * h {
        let (x, y) = (i % w, i / w);
        if (x == 0 || y == 0 || x + 1 == w || y + 1 == h) && !filled[i] {
            outside[i] = true;
            queue.push_back(i);
        }
    }
    while let Some(i) = queue.pop_front() {
        for next in neighbours(i, w, h) {
            if !filled[next] && !outside[next] {
                outside[next] = true;
                queue.push_back(next);
            }
        }
    }
    for (slot, out) in filled.iter_mut().zip(outside) {
        *slot = !out;
    }
}

#[cfg(test)]
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // test grids are small
mod tests {
    use super::*;
    use crate::colour::lab;

    /// A 100 × 100 white ROI with a disc of `skin` of radius 30 at (50, 50), a ring of `line` around it, a dark square
    /// (an "eye") inside and a skin-coloured neck below; `gap` opens the ring at the bottom.
    fn face_roi(skin: [u8; 3], line: [u8; 3], gap: bool) -> Roi {
        let (w, h) = (100, 100);
        let mut lab_px = Vec::new();
        for y in 0..h {
            for x in 0..w {
                let r = ((f64::from(x) + 0.5 - 50.0).powi(2) + (f64::from(y) + 0.5 - 50.0).powi(2)).sqrt();
                let in_gap = gap && (48..=52).contains(&x) && y > 60;
                // A neck below the face, down to the ROI's border.
                let neck = y > 80 && (42..=58).contains(&x);
                let rgb = if (f64::from(x) - 40.0).abs() < 3.0 && (f64::from(y) - 40.0).abs() < 3.0 {
                    [30, 30, 30]
                } else if r < 30.0 || (in_gap && r < 33.0) || (neck && r >= 33.0) {
                    skin
                } else if r < 33.0 {
                    line
                } else {
                    [255, 255, 255]
                };
                lab_px.push(lab(rgb).map(|v| v as f32));
            }
        }
        Roi {
            width: w as usize,
            height: h as usize,
            lab: lab_px,
            alpha: vec![255; 10_000],
            scale: 1,
            offset: [0.0, 0.0],
        }
    }

    fn frame() -> FaceFrame {
        // Eyes at (40, 40) and (60, 40): IOD 20, so the patches sit at y ≈ 48 and the chin rays reach the ring at 80.
        FaceFrame::from_eyes([40.0, 40.0], [60.0, 40.0]).unwrap()
    }

    #[test]
    fn the_skin_fills_up_to_the_line_and_the_marks_are_holes() {
        let roi = face_roi([247, 220, 203], [90, 50, 40], false);
        let seg = segment(&roi, &frame(), None).unwrap();
        let skin = lab([247, 220, 203]);
        assert!((seg.skin[0] - skin[0]).abs() < 0.01);
        assert!(seg.line_l.unwrap() < 40.0);
        assert_eq!(seg.lower_dl, -SKIN_BELOW_MAX);
        let at = |x: usize, y: usize| seg.filled[y * 100 + x];
        assert!(at(50, 50) && at(40, 40), "the eye mark is a hole, so it joins the region");
        assert!(!at(50, 82) && !at(5, 5), "the line and the white outside are not the face");
        assert!(seg.mark(40 * 100 + 40) && !seg.mark(50 * 100 + 50));
        assert_eq!(seg.background.map(|b| b[0].round()), Some(100.0));
    }

    #[test]
    fn a_gap_in_the_line_lets_the_fill_out() {
        let roi = face_roi([247, 220, 203], [90, 50, 40], true);
        let seg = segment(&roi, &frame(), None).unwrap();
        assert!(seg.filled[95 * 100 + 50], "the fill runs down the neck through the opening");
        let closed = segment(&face_roi([247, 220, 203], [90, 50, 40], false), &frame(), None).unwrap();
        assert!(!closed.filled[95 * 100 + 50], "without the opening the neck stays outside");
    }

    #[test]
    fn a_line_as_light_as_the_skin_is_refused() {
        let roi = face_roi([247, 220, 203], [215, 185, 170], false);
        match segment(&roi, &frame(), None) {
            Err(SegmentError::LowContrast(c)) => assert!(c < MIN_CONTRAST, "{c}"),
            other => panic!("expected a low-contrast refusal, got {other:?}"),
        }
    }
}
