//! The reference silhouettes the fit ops take, one per view, in any of three forms: text rows (`#` character, `.`
//! background), a PNG inline as base64 (#160's interim), or a PNG in the asset store by its content hash (a sheet view
//! the app already stored). A PNG is decoded with the `image` crate and read as a mask:
//! - when any pixel is transparent (alpha under half), the opaque pixels are the character, whatever their colour;
//! - when the image is opaque throughout, the dark pixels are (a dark figure on a light ground: luminance under half).
//!
//! Every form is checked where it enters, with an error naming the view's field: a view name, exactly one form, the
//! size (at most [`MAX_SIDE`] a side), the rows' shape and characters, the PNG's base64 and its decoding, and a mask
//! that is not blank.

use std::io::Cursor;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use image::{ImageFormat, ImageReader, Limits};
use omuzaki_ops_api::OpError;
use omuzaki_store_api::{ContentHash, Store};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{FitError, FitView, MAX_SIDE, Silhouette, Target};

/// The most views a fit takes: front, side and back.
pub const MAX_VIEWS: usize = 3;
/// The largest PNG a view may carry, in bytes (after base64 decoding): a [`MAX_SIDE`]-square RGBA image stored
/// without compression is about 1 MiB, so this leaves room for any honest silhouette.
pub const MAX_PNG_BYTES: usize = 4 << 20;
/// The most memory decoding one PNG may take, in bytes.
const MAX_DECODE_ALLOC: u64 = 16 << 20;
/// Alpha and luminance (0 to 255) at and above which a pixel is opaque, and below which it is dark.
const HALF: u8 = 128;

/// A reference silhouette: the view, and the mask in exactly one of three forms.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ViewMask {
    /// The view: `front`, `side` or `back`.
    pub view: String,
    /// The mask as text, top row first: `#` is character, `.` is background. All rows are the same length, at most
    /// 512 wide and 512 rows, and at least one pixel is `#`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rows: Option<Vec<String>>,
    /// The mask as a PNG, base64-encoded (standard alphabet, padded): opaque pixels on a transparent ground, or dark
    /// pixels on an opaque light one, are the character. At most 512 pixels a side.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub png: Option<String>,
    /// The mask as a PNG in the asset store, by its content hash; read as `png` is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub asset: Option<ContentHash>,
}

/// Where a view given as an `asset` is read.
#[derive(Clone, Copy)]
pub enum Assets<'a> {
    /// Not here: the caller is under the document lock, and reading the store hashes the whole asset. Such a view
    /// yields no target (it is read where [`Assets::Read`] is used); the bool says whether a store is wired, so the
    /// absence of one is still refused where the arguments are checked.
    Defer(bool),
    /// From this store (`None`: the app wired none, and an asset view is `Unavailable`).
    Read(Option<&'a dyn Store>),
}

/// The targets of `views`, checked for the count, the view names and each mask. A view given as an `asset` is read
/// as `assets` says.
///
/// # Errors
/// `Invalid`, naming the field, for any bad view; `NotFound` for an asset the store lacks; `Unavailable` for an asset
/// with no store.
pub fn parse_views(views: &[ViewMask], assets: Assets<'_>) -> Result<Vec<Target>, OpError> {
    if views.is_empty() || views.len() > MAX_VIEWS {
        return Err(OpError::invalid("views", format!("{} views given", views.len()))
            .hint("give one silhouette per view: front, side, back"));
    }
    let names: Vec<FitView> = views.iter().enumerate().map(|(i, v)| view_of(i, v)).collect::<Result<_, _>>()?;
    for (i, view) in names.iter().enumerate() {
        if names[..i].contains(view) {
            return Err(from_fit(&FitError::DuplicateView(*view)));
        }
    }
    let mut targets = Vec::with_capacity(views.len());
    for (i, (v, &view)) in views.iter().zip(&names).enumerate() {
        if let Some(mask) = parse_mask(i, v, assets)? {
            targets.push(Target { view, mask });
        }
    }
    Ok(targets)
}

fn view_of(i: usize, v: &ViewMask) -> Result<FitView, OpError> {
    match v.view.as_str() {
        "front" => Ok(FitView::Front),
        "side" => Ok(FitView::Side),
        "back" => Ok(FitView::Back),
        other => Err(OpError::invalid(format!("views/{i}/view"), format!("`{other}` is not a view"))
            .hint("valid views: front, side, back")),
    }
}

/// The mask of view `i`, or `None` for an asset left for later ([`Assets::Defer`]).
fn parse_mask(i: usize, v: &ViewMask, assets: Assets<'_>) -> Result<Option<Silhouette>, OpError> {
    let field = |what: &str| format!("views/{i}/{what}");
    let mask = match (&v.rows, &v.png, &v.asset) {
        (Some(rows), None, None) => parse_rows(rows, &field("rows"))?,
        (None, Some(text), None) => {
            let bytes = BASE64.decode(text.trim()).map_err(|e| {
                OpError::invalid(field("png"), format!("the base64 doesn't decode: {e}"))
                    .hint("give the PNG's bytes in standard base64, padded")
            })?;
            decode_png(&bytes, &field("png"))?
        }
        (None, None, Some(hash)) => match assets {
            Assets::Defer(true) => return Ok(None),
            Assets::Defer(false) | Assets::Read(None) => {
                return Err(OpError::unavailable("omuzaki_store_api::Store"));
            }
            Assets::Read(Some(store)) => {
                let bytes = store.get(hash).map_err(|e| {
                    OpError::not_found(format!("the asset {hash} can't be read from the store: {e}"))
                        .hint("store the sheet view first, or pass it as `png`")
                })?;
                decode_png(&bytes, &field("asset"))?
            }
        },
        _ => {
            return Err(OpError::invalid(field("rows"), "a view takes exactly one of `rows`, `png` and `asset`")
                .hint("give the mask once, in one form"));
        }
    };
    Ok(Some(mask))
}

/// A mask given as rows of `#` and `.`.
fn parse_rows(rows: &[String], field: &str) -> Result<Silhouette, OpError> {
    let height = rows.len();
    let width = rows.first().map_or(0, |r| r.chars().count());
    if height == 0 || height > MAX_SIDE || width == 0 || width > MAX_SIDE {
        return Err(OpError::invalid(field, format!("{width} x {height} is outside 1..={MAX_SIDE} on each side"))
            .hint("downsize the silhouette first"));
    }
    let mut on = Vec::with_capacity(width * height);
    for (y, row) in rows.iter().enumerate() {
        if row.chars().count() != width {
            return Err(OpError::invalid(field, format!("row {y} is not {width} wide"))
                .hint("every row is as long as the first"));
        }
        for c in row.chars() {
            on.push(match c {
                '#' => true,
                '.' => false,
                other => {
                    return Err(OpError::invalid(field, format!("row {y} holds `{other}`"))
                        .hint("rows use `#` for character and `.` for background"));
                }
            });
        }
    }
    Silhouette::new(width, height, on).map_err(|e| from_fit(&e))
}

/// A mask given as a PNG (see the [module](self) docs for how it is read).
///
/// # Errors
/// `Invalid`, naming `field`, for bytes over [`MAX_PNG_BYTES`], a PNG that doesn't decode or is over [`MAX_SIDE`] a
/// side (the decoder's limits refuse it before it allocates), or a blank mask.
pub fn decode_png(bytes: &[u8], field: &str) -> Result<Silhouette, OpError> {
    if bytes.len() > MAX_PNG_BYTES {
        return Err(OpError::invalid(field, format!("{} bytes is over {MAX_PNG_BYTES}", bytes.len()))
            .hint("downsize the silhouette first"));
    }
    let side = u32::try_from(MAX_SIDE).unwrap_or(u32::MAX);
    let mut limits = Limits::default();
    limits.max_image_width = Some(side);
    limits.max_image_height = Some(side);
    limits.max_alloc = Some(MAX_DECODE_ALLOC);
    let mut reader = ImageReader::with_format(Cursor::new(bytes), ImageFormat::Png);
    reader.limits(limits);
    let image = reader
        .decode()
        .map_err(|e| {
            OpError::invalid(field, format!("the PNG doesn't decode: {e}"))
                .hint(format!("give a PNG of at most {MAX_SIDE} pixels a side"))
        })?
        .to_rgba8();
    let (width, height) = (image.width() as usize, image.height() as usize);
    let see_through = image.pixels().any(|p| p[3] < HALF);
    let on = image
        .pixels()
        .map(|p| {
            if see_through {
                p[3] >= HALF
            } else {
                let luminance = (299 * u32::from(p[0]) + 587 * u32::from(p[1]) + 114 * u32::from(p[2])) / 1000;
                luminance < u32::from(HALF)
            }
        })
        .collect();
    Silhouette::new(width, height, on).map_err(|e| {
        OpError::invalid(field, e.to_string())
            .hint("a transparent ground with an opaque figure, or a dark figure on a light ground")
    })
}

/// The refusal for a fit error: the input is at fault, except when a candidate body cannot be built.
pub fn from_fit(e: &FitError) -> OpError {
    match e {
        FitError::NoTargets | FitError::DuplicateView(_) => {
            OpError::invalid("views", e.to_string()).hint("give one silhouette per view, at least one")
        }
        FitError::BadMask(_) => OpError::invalid("views", e.to_string()).hint("give a mask with some `#` pixels"),
        _ => OpError::internal(format!("the fit failed: {e}")),
    }
}

#[cfg(test)]
mod tests {
    use image::{ImageEncoder, codecs::png::PngEncoder};

    use super::*;

    fn png_of(width: u32, height: u32, pixel: impl Fn(u32, u32) -> [u8; 4]) -> Vec<u8> {
        let data: Vec<u8> =
            (0..height).flat_map(|y| (0..width).map(move |x| (x, y))).flat_map(|(x, y)| pixel(x, y)).collect();
        let mut out = Vec::new();
        PngEncoder::new(&mut out).write_image(&data, width, height, image::ExtendedColorType::Rgba8).unwrap();
        out
    }

    #[test]
    fn a_transparent_ground_reads_by_alpha_and_an_opaque_one_by_darkness() {
        // A red figure on a transparent ground: red is not dark, but it is opaque.
        let alpha = png_of(4, 3, |x, _| if x == 1 { [200, 30, 30, 255] } else { [255, 255, 255, 0] });
        let m = decode_png(&alpha, "views/0/png").unwrap();
        assert_eq!((m.width(), m.height()), (4, 3));
        assert!(m.pixels().iter().enumerate().all(|(i, &on)| on == (i % 4 == 1)));
        // A black figure on white, no transparency.
        let ink = png_of(4, 3, |x, _| if x == 2 { [10, 10, 10, 255] } else { [250, 250, 250, 255] });
        let m = decode_png(&ink, "views/0/png").unwrap();
        assert!(m.pixels().iter().enumerate().all(|(i, &on)| on == (i % 4 == 2)));
    }

    #[test]
    fn bad_pngs_are_refused_by_their_field() {
        let e = decode_png(b"not a png", "views/1/png").unwrap_err();
        assert_eq!(e.field.as_deref(), Some("views/1/png"));
        assert!(e.message.contains("doesn't decode"), "{}", e.message);
        let side = u32::try_from(MAX_SIDE).unwrap() + 1;
        let e = decode_png(&png_of(side, 2, |_, _| [0, 0, 0, 255]), "views/0/png").unwrap_err();
        assert!(e.message.contains("doesn't decode"), "{}", e.message);
        let e = decode_png(&png_of(3, 3, |_, _| [255, 255, 255, 255]), "views/0/png").unwrap_err();
        assert!(e.message.contains("blank"), "{}", e.message);
        let e = decode_png(&vec![0; MAX_PNG_BYTES + 1], "views/0/png").unwrap_err();
        assert!(e.message.contains("over"), "{}", e.message);
    }
}
