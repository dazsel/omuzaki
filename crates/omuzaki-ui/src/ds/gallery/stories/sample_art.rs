//! Stand-in pictures for the stories that show thumbnails: a soft gradient with a disc, made once per theme and kept in
//! the context. They are synthetic on purpose, so a screenshot never carries a person's picture or a character.

use egui::{ColorImage, Context, Id, TextureHandle, TextureId, TextureOptions};

use crate::ds::tokens::Theme;

const WIDTH: usize = 64;
const HEIGHT: usize = 48;

/// The texture of picture `variant` (0, 1 or 2) in `theme`'s colours, made on first use.
pub fn texture(ctx: &Context, theme: &Theme, variant: u8) -> TextureId {
    let key = Id::new(("gallery_art", variant, theme.kind));
    if let Some(handle) = ctx.data(|d| d.get_temp::<TextureHandle>(key)) {
        return handle.id();
    }
    let handle = ctx.load_texture(
        format!("gallery-art-{variant}-{}", theme.kind.slug()),
        picture(theme, variant),
        TextureOptions::LINEAR,
    );
    let id = handle.id();
    ctx.data_mut(|d| d.insert_temp(key, handle));
    id
}

/// A vertical gradient from `bg3` to `bg4` with a disc whose place and colour depend on `variant`.
fn picture(theme: &Theme, variant: u8) -> ColorImage {
    let disc = [theme.accent, theme.agent, theme.ok][usize::from(variant % 3)];
    let centre = [(22.0, 24.0), (42.0, 22.0), (32.0, 28.0)][usize::from(variant % 3)];
    let mut pixels = Vec::with_capacity(WIDTH * HEIGHT);
    for y in 0..HEIGHT {
        #[allow(clippy::cast_precision_loss, reason = "pixel coordinates")]
        let t = y as f32 / (HEIGHT - 1) as f32;
        let ground = theme.bg3.lerp_to_gamma(theme.bg4, t);
        for x in 0..WIDTH {
            #[allow(clippy::cast_precision_loss, reason = "pixel coordinates")]
            let (dx, dy) = (x as f32 - centre.0, y as f32 - centre.1);
            let inside = (dx * dx + dy * dy).sqrt();
            // A one-pixel soft edge.
            let cover = (11.0 - inside).clamp(0.0, 1.0);
            pixels.push(ground.lerp_to_gamma(disc, cover));
        }
    }
    ColorImage { size: [WIDTH, HEIGHT], source_size: egui::vec2(WIDTH as f32, HEIGHT as f32), pixels }
}
