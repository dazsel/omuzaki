//! Textures: a PNG of the request's size clamped like images. A tileable one is a gradient on the distance to the
//! nearest edge, so the left column equals the right one and the top row the bottom one, pixel for pixel; otherwise
//! it is the image gradient.

use omuzaki_connect_api::{Capability, ConnectError, Output, Request};

use crate::fake::{Generator, seed_of, wrong_kind};
use crate::gen_image::{clamp, colours, image_artifact, mix, png};

/// The model fake textures report.
pub const MODEL: &str = "mock-texture@1";

pub(crate) struct TextureGen;

impl Generator for TextureGen {
    fn capability(&self) -> Capability {
        Capability::Texture
    }

    fn make(&self, req: &Request, index: u32) -> Result<Output, ConnectError> {
        let Request::Texture(ask) = req else {
            return Err(wrong_kind(Capability::Texture, req));
        };
        let seed = seed_of(req, index);
        let (width, height) = (clamp(ask.width), clamp(ask.height));
        let [from, to] = colours(seed);
        let bytes = if ask.tileable {
            // Distance to the nearer of two opposite edges is the same at both edges.
            let fold = |v: u32, side: u32| v.min(side - 1 - v);
            let span = (width / 2 + height / 2).max(1);
            png(width, height, |x, y| mix(from, to, fold(x, width) + fold(y, height), span))?
        } else {
            let span = (width + height).saturating_sub(2).max(1);
            png(width, height, |x, y| mix(from, to, x + y, span))?
        };
        Ok(image_artifact(bytes, MODEL, seed))
    }
}
