//! Images: a PNG gradient whose two colours come from the seed, at the request's size clamped to [`MAX_SIDE`] (tests
//! stay fast). Integer arithmetic only, so the bytes are the same on every platform.

use omuzaki_connect_api::{Artifact, Capability, ConnectError, MediaKind, Output, Request};
use omuzaki_core::rng::Rng;

use crate::fake::{Generator, seed_of, wrong_kind};

/// The largest side a fake image has, in pixels.
pub const MAX_SIDE: u32 = 64;

/// The model fake images report.
pub const MODEL: &str = "mock-image@1";

pub(crate) struct ImageGen;

impl Generator for ImageGen {
    fn capability(&self) -> Capability {
        Capability::Image
    }

    fn make(&self, req: &Request, index: u32) -> Result<Output, ConnectError> {
        let Request::Image(ask) = req else {
            return Err(wrong_kind(Capability::Image, req));
        };
        let seed = seed_of(req, index);
        let (width, height) = (clamp(ask.width), clamp(ask.height));
        let [from, to] = colours(seed);
        // The gradient runs along x + y, so every pixel of a side differs from the opposite side's.
        let span = (width + height).saturating_sub(2).max(1);
        let bytes = png(width, height, |x, y| mix(from, to, x + y, span))?;
        Ok(image_artifact(bytes, MODEL, seed))
    }
}

/// A side clamped to 1..=[`MAX_SIDE`].
pub(crate) fn clamp(side: u32) -> u32 {
    side.clamp(1, MAX_SIDE)
}

/// Two colours drawn from `seed`.
pub(crate) fn colours(seed: u64) -> [[u8; 3]; 2] {
    let mut rng = Rng::new(seed);
    let mut colour = || {
        let bits = rng.next_u64().to_le_bytes();
        [bits[0], bits[1], bits[2]]
    };
    [colour(), colour()]
}

/// `a` at `t = 0` to `b` at `t = span`, per channel.
pub(crate) fn mix(a: [u8; 3], b: [u8; 3], t: u32, span: u32) -> [u8; 3] {
    let t = t.min(span);
    let channel = |i: usize| {
        let value = (u32::from(a[i]) * (span - t) + u32::from(b[i]) * t) / span;
        u8::try_from(value).unwrap_or(u8::MAX)
    };
    [channel(0), channel(1), channel(2)]
}

/// An RGB PNG of `w` by `h` from a pixel function.
pub(crate) fn png(w: u32, h: u32, pixel: impl Fn(u32, u32) -> [u8; 3]) -> Result<Vec<u8>, ConnectError> {
    let img = image::RgbImage::from_fn(w, h, |x, y| image::Rgb(pixel(x, y)));
    let mut bytes = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut bytes), image::ImageFormat::Png)
        .map_err(|e| ConnectError::failed(format!("the fake could not encode its PNG: {e}")))?;
    Ok(bytes)
}

/// A PNG artifact.
pub(crate) fn image_artifact(bytes: Vec<u8>, model: &str, seed: u64) -> Output {
    Output::Asset(Artifact {
        bytes,
        media_type: "image/png".into(),
        kind: MediaKind::Image,
        name: None,
        model: model.into(),
        seed: Some(seed),
        rights: None,
    })
}
