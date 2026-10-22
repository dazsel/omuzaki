//! The skirt's plaid: the prototype's `plaidTexture` (`uniform.js` 377-401), drawn as a vector picture.
//!
//! A 256 x 256 canvas of 2 x 2 repeats of a 128-pixel tile, in four colours: the base, a deeper tone for the broad bands, a
//! line colour, and a hairline colour. Each tile has, in this order, in both directions (the vertical one first): a broad
//! band at 62 %, thin lines either side of it at 95 % and 50 %, and a faint hairline down its middle at 28 %; the
//! twill weave goes over all of it as diagonal lines at 7 %. The picture is drawn in canvas pixels, rows top to bottom as
//! the canvas is, and the rasteriser paints it at any resolution: a texture of 512 pixels is the same picture with every
//! pixel constant doubled. (The prototype flips its canvas when it uploads it and counts `v` upward; the mesh here counts
//! `v` downward from the top row, as glTF does, so the picture needs no flip.)

use omuzaki_core::colour::Colour;
use omuzaki_core::glam::{Affine2, UVec2, Vec2};
use omuzaki_face_api::{Cap, Cmd, DrawList, FaceRect, Join, Paint};
use omuzaki_render_api::{Filter, Sampler, TextureDesc, Wrap};

/// The canvas's side, drawing units (pixels at the prototype's resolution).
pub const CANVAS: f32 = 256.0;
/// The same as whole pixels, for the weave's loop.
const CANVAS_PX: i32 = 256;
/// The side of one repeat of the pattern.
const PERIOD: f32 = 128.0;
/// The broad band's start and width, and the opacity it is drawn at.
const BAND: (f32, f32, f32) = (34.0, 58.0, 0.62);
/// The thin lines: their width, where each pair starts and their opacity, for the outer pair and the inner pair.
const LINES: [(f32, [f32; 2], f32); 2] = [(2.5, [16.0, 104.0], 0.95), (1.0, [23.0, 111.0], 0.5)];
/// The hairline's start, width and opacity.
const HAIR: (f32, f32, f32) = (63.0, 1.2, 0.28);
/// The twill weave's opacity, line width and the spacing of its lines.
const TWILL: (f32, f32, i32) = (0.07, 1.0, 4);
/// The anisotropy the texture is read with.
const ANISOTROPY: u8 = 8;
/// The fewest and most pixels a side of the texture may have.
pub const MIN_RESOLUTION: u32 = 64;
/// See [`MIN_RESOLUTION`].
pub const MAX_RESOLUTION: u32 = 1024;
/// The prototype's resolution.
pub const DEFAULT_RESOLUTION: u32 = 256;

/// The four colours of the plaid.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Colours {
    /// The ground.
    pub base: Colour,
    /// The broad bands.
    pub band: Colour,
    /// The thin lines.
    pub line: Colour,
    /// The hairline.
    pub hairline: Colour,
}

/// A filled rectangle of the canvas.
fn rect(list: &mut DrawList, colour: Colour, [x, y, w, h]: [f32; 4], opacity: f32) {
    let corners = [Vec2::new(x, y), Vec2::new(x + w, y), Vec2::new(x + w, y + h), Vec2::new(x, y + h)];
    let path = list.path().polyline(&corners).close().finish();
    list.push(Cmd::Fill { path, paint: Paint::Solid(colour), opacity });
}

/// A rectangle and the same one turned across the diagonal: the vertical stripe, then the horizontal.
fn both(list: &mut DrawList, colour: Colour, [x, y, w, h]: [f32; 4], opacity: f32) {
    rect(list, colour, [x, y, w, h], opacity);
    rect(list, colour, [y, x, h, w], opacity);
}

/// The plaid as a drawing on a [`CANVAS`] square.
pub fn draw(colours: &Colours) -> DrawList {
    let mut list = DrawList::new();
    rect(&mut list, colours.base, [0.0, 0.0, CANVAS, CANVAS], 1.0);
    for ox in [0.0, PERIOD] {
        for oy in [0.0, PERIOD] {
            list.push(Cmd::PushTransform(Affine2::from_translation(Vec2::new(ox, oy))));
            both(&mut list, colours.band, [BAND.0, 0.0, BAND.1, PERIOD], BAND.2);
            for (width, starts, opacity) in LINES {
                for x in starts {
                    both(&mut list, colours.line, [x, 0.0, width, PERIOD], opacity);
                }
            }
            both(&mut list, colours.hairline, [HAIR.0, 0.0, HAIR.1, PERIOD], HAIR.2);
            list.push(Cmd::PopTransform);
        }
    }
    // The lines start a canvas to the left and end at its right edge, in steps of the weave's spacing.
    let mut k = -CANVAS_PX;
    while k < CANVAS_PX {
        #[allow(clippy::cast_precision_loss)] // a few hundred
        let (from, to) = (Vec2::new(k as f32, 0.0), Vec2::new(k as f32 + CANVAS, CANVAS));
        let path = list.path().move_to(from).line_to(to).finish();
        list.push(Cmd::Stroke {
            path,
            paint: Paint::Solid(Colour::rgb(0, 0, 0)),
            width: TWILL.1,
            cap: Cap::Butt,
            join: Join::Miter,
            opacity: TWILL.0,
        });
        k += TWILL.2;
    }
    list
}

/// The drawing's plane: the canvas.
pub fn plane() -> FaceRect {
    FaceRect { x0: 0.0, x1: CANVAS, y0: 0.0, y1: CANVAS }
}

/// How the plaid texture of `resolution` pixels a side is made and read: sRGB, repeating, mip-mapped, filtered.
pub fn desc(resolution: u32) -> TextureDesc {
    let mut desc = TextureDesc::colour(UVec2::splat(resolution));
    desc.sampler = Sampler { filter: Filter::Linear, wrap: Wrap::Repeat, anisotropy: ANISOTROPY };
    desc
}
