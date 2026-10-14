//! Underlay tracing (spec CB-25): the locked sheet's face drawn over the face rectangle at any opacity, so an artist can
//! trace the features 1:1 (`face.underlay.set`).
//!
//! The underlay is a stage-level overlay on the head material, not a face layer. The viewport composites it **before**
//! the `Base` layer, so the painted skin covers it wherever the skin is opaque. It uses the Base layer's window, fade and
//! texture size, so it lands exactly where the painted face does. Its pixels are the sheet's face cropped and rescaled to
//! that resolution, premultiplied sRGB like every face layer.
//!
//! Opacity is the overlay's strength: changing it is a material update, never a repaint. Alignment changes the pixels, so
//! it rebuilds the layer. That rebuild runs on change, not per frame, and is the only place this module allocates.
//!
//! `face.underlay.set` is view state, not a document edit. Its op registration and the stage wiring are separate work; the request
//! and view types land here, so the op is a thin wrapper over them.

use omuzaki_char_api::{CharId, face_texture_desc};
use omuzaki_core::glam::{UVec2, Vec3};
use omuzaki_face_api::{FaceError, FaceLayerId, FaceLayerSpec, FaceProjection, PlanarWindow, narrow};
use omuzaki_ops_api::OpError;
use omuzaki_render_api::{Mapping, Overlay, PixelRect, TextureDesc, TextureId, limits};
use omuzaki_store_api::ContentHash;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The locked sheet: straight-alpha sRGB RGBA8, rows top to bottom, as the asset store holds it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sheet<'a> {
    size: UVec2,
    rgba: &'a [u8],
}

impl<'a> Sheet<'a> {
    /// The sheet of `size` pixels with `rgba` pixels.
    ///
    /// # Errors
    /// [`FaceError::Invalid`] naming `sheet_size` for a zero side, or `sheet_rgba` when its length is not
    /// `4 * width * height`.
    pub fn new(size: UVec2, rgba: &'a [u8]) -> Result<Self, FaceError> {
        if size.x == 0 || size.y == 0 {
            return Err(FaceError::Invalid(format!(
                "sheet_size is {}x{}; both sides must be at least 1",
                size.x, size.y
            )));
        }
        let expected = byte_len(size)
            .ok_or_else(|| FaceError::Invalid(format!("sheet_size {}x{} is too large", size.x, size.y)))?;
        if rgba.len() != expected {
            return Err(FaceError::Invalid(format!(
                "sheet_rgba has {} bytes; a {}x{} RGBA sheet needs {expected}",
                rgba.len(),
                size.x,
                size.y
            )));
        }
        Ok(Self { size, rgba })
    }
}

/// Bytes of an RGBA8 image of `size`, or `None` when that overflows `usize`.
fn byte_len(size: UVec2) -> Option<usize> {
    usize::try_from(size.x).ok()?.checked_mul(usize::try_from(size.y).ok()?)?.checked_mul(4)
}

/// Fine registration of the sheet's picture over the face, in sheet pixels. The fields describe the picture's transform:
/// `offset` `+x` moves it right, `scale` 2 makes it twice as large, and a positive `rotation` turns it from `+x` toward
/// `+y` (y down). All three act about the face centre, in that order: scale and rotation, then offset.
/// [`UnderlayAlign::IDENTITY`] leaves the picture where the sheet has it, and at the sheet's own pixel scale that is a
/// 1:1 copy.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UnderlayAlign {
    /// Shift of the picture, in sheet pixels. `+x` moves it right, `+y` down.
    pub offset: [f64; 2],
    /// Scale of the picture about the face centre: 2 doubles it, 1 keeps the sheet's face at the rectangle's size. Must
    /// be above zero.
    pub scale: f64,
    /// Rotation of the picture about the face centre, in radians. Positive turns `+x` toward `+y` (y down).
    pub rotation: f64,
}

impl UnderlayAlign {
    /// No correction.
    pub const IDENTITY: Self = Self { offset: [0.0, 0.0], scale: 1.0, rotation: 0.0 };

    /// Checks every field is usable: finite values, and a scale above zero (a zero scale has no inverse).
    ///
    /// # Errors
    /// [`OpError`] naming `align.offset`, `align.scale` or `align.rotation` for the first field at fault.
    pub fn check(&self) -> Result<(), OpError> {
        if !self.offset.iter().all(|v| v.is_finite()) {
            return Err(OpError::invalid(
                "align.offset",
                format!("align.offset is {:?}; both values must be finite", self.offset),
            ));
        }
        if !self.rotation.is_finite() {
            return Err(OpError::invalid(
                "align.rotation",
                format!("align.rotation is {}; it must be finite", self.rotation),
            ));
        }
        if !(self.scale.is_finite() && self.scale > 0.0) {
            return Err(OpError::invalid(
                "align.scale",
                format!("align.scale is {}; it must be finite and above zero", self.scale),
            ));
        }
        Ok(())
    }
}

/// Where a layer pixel reads the sheet: the sheet's face rectangle sized to the layer, then the inverse of the picture's
/// alignment (see [`UnderlayAlign`]). Both directions are continuous (pixel `i` covers `i..i + 1`), so `to_sheet` of a
/// pixel centre is the point to sample.
///
/// The maths is `f64`: in `f32` a coordinate near 1500 px already rounds by about 1e-4 px, the tolerance the round trip
/// must keep.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Registration {
    layer_centre: [f64; 2],
    sheet_centre: [f64; 2],
    /// Sheet pixels per layer pixel, per axis.
    per_px: [f64; 2],
    align: UnderlayAlign,
    cos: f64,
    sin: f64,
}

impl Registration {
    /// The map for a layer of `layer_px` pixels over `sheet_face` of the sheet, moved by `align`. The caller checks
    /// `align` with [`UnderlayAlign::check`] and `sheet_face` with [`Underlay::new`].
    pub fn new(sheet_face: PixelRect, layer_px: UVec2, align: UnderlayAlign) -> Self {
        let (sin, cos) = align.rotation.sin_cos();
        let (width, height) = (f64::from(sheet_face.width), f64::from(sheet_face.height));
        Self {
            layer_centre: [f64::from(layer_px.x) / 2.0, f64::from(layer_px.y) / 2.0],
            sheet_centre: [f64::from(sheet_face.x) + width / 2.0, f64::from(sheet_face.y) + height / 2.0],
            per_px: [width / f64::from(layer_px.x), height / f64::from(layer_px.y)],
            align,
            cos,
            sin,
        }
    }

    /// The sheet point, in continuous sheet pixels, that layer point `layer` shows. With `c` the face centre, `K` the
    /// sheet pixels per layer pixel, `C` the layer centre and `R(-r)` the rotation by minus the alignment's rotation,
    /// this is `c + R(-r) (K (layer - C) - offset) / scale`: the inverse of the picture's transform, so the picture
    /// lands where the alignment says.
    pub fn to_sheet(&self, layer: [f64; 2]) -> [f64; 2] {
        let v = [
            (layer[0] - self.layer_centre[0]) * self.per_px[0] - self.align.offset[0],
            (layer[1] - self.layer_centre[1]) * self.per_px[1] - self.align.offset[1],
        ];
        // R(-r) is the transpose of R(r), and the scale is undone last.
        let r = [self.cos * v[0] + self.sin * v[1], -self.sin * v[0] + self.cos * v[1]];
        let scale = self.align.scale;
        [self.sheet_centre[0] + r[0] / scale, self.sheet_centre[1] + r[1] / scale]
    }

    /// The layer point that shows sheet point `sheet`: the exact inverse of [`Registration::to_sheet`], that is the
    /// picture's transform (scale and rotate about the centre, then shift) mapped back onto the layer grid.
    pub fn to_layer(&self, sheet: [f64; 2]) -> [f64; 2] {
        let u = [sheet[0] - self.sheet_centre[0], sheet[1] - self.sheet_centre[1]];
        let r = [self.cos * u[0] - self.sin * u[1], self.sin * u[0] + self.cos * u[1]];
        let scale = self.align.scale;
        let d = [self.align.offset[0] + scale * r[0], self.align.offset[1] + scale * r[1]];
        [self.layer_centre[0] + d[0] / self.per_px[0], self.layer_centre[1] + d[1] / self.per_px[1]]
    }
}

/// Builds underlays. `Underlay::new` is the one constructor; the layer it returns owns the pixels.
pub struct Underlay;

impl Underlay {
    /// The underlay of `sheet`: the pixels of `sheet_face` (the sheet's face rectangle, in sheet pixels) at the Base
    /// layer's resolution (`px_per_m`), moved by `align`. Its window is the Base layer's window over `projection` for a
    /// head centred at `head_centre` in bind space, and its opacity starts at 1.
    ///
    /// The sample at each layer pixel centre is bilinear over premultiplied colour, so a half-transparent sheet pixel
    /// does not darken its neighbours. At identity alignment and a sheet face the same size as the layer, every layer
    /// pixel reads exactly one sheet pixel (the 1:1 case).
    ///
    /// # Errors
    /// [`FaceError::Invalid`] naming `sheet_face` when it is empty or leaves the sheet; naming `px_per_m` when it is not
    /// finite and above zero, or when the layer's side would exceed [`limits::GUARANTEED_TEXTURE_SIZE`]; or naming the
    /// `align` field at fault (see [`UnderlayAlign::check`]). Every check runs before the pixel buffer is allocated.
    // The name is deliberate: this constructor builds the layer, so the return type is not `Self`.
    #[allow(clippy::new_ret_no_self)]
    pub fn new(
        sheet: &Sheet<'_>,
        sheet_face: PixelRect,
        projection: &FaceProjection,
        head_centre: Vec3,
        px_per_m: f32,
        align: &UnderlayAlign,
    ) -> Result<UnderlayLayer, FaceError> {
        check_face(sheet, sheet_face)?;
        align.check().map_err(|e| FaceError::Invalid(e.to_string()))?;
        // Checked before sizing: a NaN or zero density would otherwise round the layer to 1x1 without complaint.
        if !(px_per_m.is_finite() && px_per_m > 0.0) {
            return Err(FaceError::Invalid(format!("px_per_m is {px_per_m}; it must be finite and above zero")));
        }
        let spec = FaceLayerSpec::for_layer(FaceLayerId::Base, projection, px_per_m);
        let (w, h) = spec.size_px();
        let limit = limits::GUARANTEED_TEXTURE_SIZE;
        if w > limit || h > limit {
            return Err(FaceError::Invalid(format!(
                "px_per_m {px_per_m} makes the underlay {w}x{h} px; a side may be at most {limit} px"
            )));
        }
        let texture_desc = face_texture_desc(&spec);
        let registration = Registration::new(sheet_face, UVec2::new(w, h), *align);
        let mut rgba = Vec::with_capacity(texture_desc.byte_len());
        for y in 0..h {
            for x in 0..w {
                let at = [f64::from(x) + 0.5, f64::from(y) + 0.5];
                rgba.extend_from_slice(&sample(sheet, registration.to_sheet(at)));
            }
        }
        Ok(UnderlayLayer {
            texture_desc,
            rgba,
            window: projection.planar(FaceLayerId::Base, head_centre),
            opacity: 1.0,
        })
    }
}

/// Rejects a face rectangle that is empty or reaches past the sheet, naming the field at fault.
fn check_face(sheet: &Sheet<'_>, face: PixelRect) -> Result<(), FaceError> {
    if face.width > 0 && face.height > 0 && face.fits(sheet.size) {
        return Ok(());
    }
    Err(FaceError::Invalid(format!(
        "sheet_face is {}x{} at ({}, {}); it must be non-empty and inside the {}x{} sheet",
        face.width, face.height, face.x, face.y, sheet.size.x, sheet.size.y
    )))
}

/// Bilinear sample of the sheet at continuous point `at`, premultiplied and rounded to `0..=255`. Texel centres sit at
/// half-integers, so the lookup shifts by half a pixel. A texel outside the sheet is transparent: the underlay fades out
/// at the sheet's edge instead of repeating its border.
#[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // coordinates are sheet pixels, far inside i64; the
// value is clamped to 0..=255 first, so the cast is exact
fn sample(sheet: &Sheet<'_>, at: [f64; 2]) -> [u8; 4] {
    let (tx, ty) = (at[0] - 0.5, at[1] - 0.5);
    let (x0, y0) = (tx.floor(), ty.floor());
    let (fx, fy) = (tx - x0, ty - y0);
    // `as` saturates rather than wrapping, so a far-off point simply reads transparent.
    let (ix, iy) = (x0 as i64, y0 as i64);
    let mut acc = [0.0_f64; 4];
    for (dy, wy) in [(0, 1.0 - fy), (1, fy)] {
        for (dx, wx) in [(0, 1.0 - fx), (1, fx)] {
            let Some(texel) = texel(sheet, ix + dx, iy + dy) else { continue };
            let w = wx * wy;
            let alpha = f64::from(texel[3]) / 255.0;
            for (sum, &channel) in acc.iter_mut().zip(&texel).take(3) {
                *sum += w * f64::from(channel) * alpha;
            }
            acc[3] += w * f64::from(texel[3]);
        }
    }
    acc.map(|v| v.round().clamp(0.0, 255.0) as u8)
}

/// The straight-alpha texel at `column`, `row`, or `None` outside the sheet.
fn texel(sheet: &Sheet<'_>, column: i64, row: i64) -> Option<[u8; 4]> {
    let (width, height) = (i64::from(sheet.size.x), i64::from(sheet.size.y));
    if !(0..width).contains(&column) || !(0..height).contains(&row) {
        return None;
    }
    let index = usize::try_from(row * width + column).ok()?;
    let bytes = sheet.rgba.get(index * 4..index * 4 + 4)?;
    Some([bytes[0], bytes[1], bytes[2], bytes[3]])
}

/// The underlay's pixels, window and opacity, ready to composite over the head.
#[derive(Clone, Debug, PartialEq)]
pub struct UnderlayLayer {
    texture_desc: TextureDesc,
    rgba: Vec<u8>,
    window: PlanarWindow,
    opacity: f32,
}

impl UnderlayLayer {
    /// The texture's shape: the Base layer's size, premultiplied sRGB, dynamic so a new alignment repaints it.
    pub fn texture_desc(&self) -> &TextureDesc {
        &self.texture_desc
    }

    /// Premultiplied sRGB RGBA8 pixels, rows top to bottom, `texture_desc().byte_len()` bytes.
    pub fn rgba(&self) -> &[u8] {
        &self.rgba
    }

    /// Where the underlay lands in bind space: the Base layer's window.
    pub fn window(&self) -> PlanarWindow {
        self.window
    }

    /// The overlay strength, `0..=1`. Zero hides the underlay.
    pub fn opacity(&self) -> f32 {
        self.opacity
    }

    /// Sets the overlay strength. It changes no pixels, so it is a material update, not a repaint.
    ///
    /// # Errors
    /// [`FaceError::Invalid`] naming `opacity` unless it is a number within `0..=1`.
    pub fn set_opacity(&mut self, opacity: f64) -> Result<(), FaceError> {
        if !opacity_in_range(opacity) {
            return Err(FaceError::Invalid(format!("opacity is {opacity}; it must be between 0 and 1")));
        }
        self.opacity = narrow(opacity);
        Ok(())
    }

    /// The renderer's overlay of this layer on `texture`, with the opacity as its strength. Put it before the Base
    /// overlay in the head material, so the painted skin covers it.
    pub fn overlay(&self, texture: TextureId) -> Overlay {
        let w = self.window;
        Overlay {
            texture,
            mapping: Mapping::Planar { min: w.min, max: w.max, fade_from: w.fade_from, fade_to: w.fade_to },
            strength: self.opacity,
        }
    }
}

/// `true` for a number within `0..=1`. NaN fails the comparison, so it is rejected too.
fn opacity_in_range(opacity: f64) -> bool {
    (0.0..=1.0).contains(&opacity)
}

/// What the viewport applies after `face.underlay.set`: the window and strength, not the pixels, which stay with the
/// layer. Real-time values are `f32`, as in every view type.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct UnderlayView {
    /// Lower-left corner of the window, in bind-space metres.
    pub min: [f32; 2],
    /// Upper-right corner of the window.
    pub max: [f32; 2],
    /// Bind-space `z` where the underlay starts to show.
    pub fade_from: f32,
    /// Bind-space `z` where it is fully shown.
    pub fade_to: f32,
    /// Overlay strength, `0..=1`.
    pub opacity: f32,
}

impl UnderlayView {
    /// The description of `layer`.
    pub fn of(layer: &UnderlayLayer) -> Self {
        let w = layer.window;
        Self {
            min: w.min.to_array(),
            max: w.max.to_array(),
            fade_from: w.fade_from,
            fade_to: w.fade_to,
            opacity: layer.opacity,
        }
    }
}

/// The arguments of `face.underlay.set`: whose head is traced, which sheet, the strength and the alignment. It changes
/// no document and returns an [`UnderlayView`].
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UnderlaySet {
    /// The character whose head the sheet is traced onto.
    pub character: CharId,
    /// The sheet image, by content hash.
    pub asset: ContentHash,
    /// Overlay strength, `0..=1`; zero hides the underlay.
    pub opacity: f64,
    /// Fine registration of the sheet over the face.
    pub align: UnderlayAlign,
}

impl UnderlaySet {
    /// Checks the arguments before anything is built.
    ///
    /// # Errors
    /// [`OpError`] naming `opacity` when it is not a number within `0..=1`, or the `align` field at fault.
    pub fn validate(&self) -> Result<(), OpError> {
        if !opacity_in_range(self.opacity) {
            return Err(OpError::invalid(
                "opacity",
                format!("opacity is {}; it must be between 0 and 1", self.opacity),
            ));
        }
        self.align.check()
    }
}
