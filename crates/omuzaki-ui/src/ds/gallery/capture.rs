//! Headless rendering of the gallery: PNG screenshots and a frame-time measurement, with no window.
//!
//! It drives `egui_kittest`'s wgpu renderer, which prefers a software rasteriser (lavapipe) when one exists, so
//! screenshots are the same on every machine. The `gallery` binary calls [`render`]; tests call it to check pixels.

use egui::{Vec2, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable as _;
use omuzaki_core::colour::Colour;

use super::{Gallery, Group};
use crate::ds::theme::{Settings, apply_settings};
use crate::ds::tokens::Theme;
use crate::ds::widgets::{ColourField, PaletteSlot};

/// What to render.
#[derive(Clone, Copy, Debug)]
pub struct Shot {
    /// Theme, scale, density, motion.
    pub settings: Settings,
    /// Window size in points.
    pub size: Vec2,
    /// Output pixels per point (2 for a crisp 200% capture of a 100% layout).
    pub pixels_per_point: f32,
    /// Speak the pseudo-locale (accented, 40% longer) instead of English.
    pub pseudo: bool,
    /// Only this group's stories (`None`: the whole gallery).
    pub group: Option<Group>,
    /// Only this part of the group, counted from 0 (`None`: the whole group); see [`Gallery::with_group`].
    pub part: Option<(usize, usize)>,
}

impl Shot {
    /// A 1680 x 1050 pt window at 1 px per point, the whole gallery.
    pub fn new(settings: Settings) -> Self {
        Self { settings, size: vec2(1680.0, 1050.0), pixels_per_point: 1.0, pseudo: false, group: None, part: None }
    }

    /// The gallery this shot shows, before it is laid out.
    fn gallery(self) -> Gallery {
        Gallery::new(self.settings).with_pseudo(self.pseudo).with_group(self.group, self.part)
    }
}

/// The window height a fitted gallery is first laid out in: taller than any section, so nothing is cut off while the
/// content height is measured.
const MEASURE_HEIGHT: f32 = 8000.0;

/// The height, in points, that the gallery of `shot` needs to show every card (its settings, language and group;
/// the window's own size and pixel density do not change it). Lays it out without a GPU, so tests can call it for
/// every group.
pub fn content_height(shot: Shot) -> f32 {
    let mut harness = Harness::builder()
        .with_size(vec2(shot.size.x, MEASURE_HEIGHT))
        .with_pixels_per_point(shot.pixels_per_point)
        .build_ui_state(|ui, gallery: &mut Gallery| gallery.ui(ui), shot.gallery());
    harness.run_steps(4);
    harness.run_ok();
    harness.run_steps(2);
    harness.run_ok();
    harness.state().content_height().ceil()
}

/// Renders the gallery once the layout has settled.
///
/// A window height of zero (or less) means "as tall as the content": the gallery is laid out once in a very tall
/// window to measure it, then rendered at exactly that height, so one image shows every card.
///
/// # Errors
/// Fails when no GPU adapter (hardware or software) is available, or the render fails.
pub fn render(shot: Shot) -> Result<image::RgbaImage, String> {
    let fit = shot.size.y <= 0.0;
    let size = if fit { vec2(shot.size.x, MEASURE_HEIGHT) } else { shot.size };
    let mut harness = Harness::builder()
        .with_size(size)
        .with_pixels_per_point(shot.pixels_per_point)
        .wgpu()
        .build_ui_state(|ui, gallery: &mut Gallery| gallery.ui(ui), shot.gallery());
    // Fonts and the first layout need a few passes; running to stability also finishes hover fades.
    harness.run_steps(4);
    harness.run_ok();
    if fit {
        harness.set_size(vec2(shot.size.x, harness.state().content_height().ceil()));
        harness.run_steps(2);
        harness.run_ok();
    }
    harness.render()
}

/// The colour field with its popover open, on a panel: the one gallery state a static page cannot show.
///
/// # Errors
/// Fails when no GPU adapter is available, or the render fails.
pub fn colour_popover(settings: Settings, pixels_per_point: f32) -> Result<image::RgbaImage, String> {
    let mut colour = Colour::rgb(0x8A, 0x6F, 0xD1);
    let palette = [
        PaletteSlot { name: "hair.root", colour: Colour::rgb(0x8A, 0x6F, 0xD1) },
        PaletteSlot { name: "hair.tip", colour: Colour::rgb(0xE6, 0xC3, 0x6A) },
        PaletteSlot { name: "skin.base", colour: Colour::rgb(0xF2, 0xC9, 0xB0) },
        PaletteSlot { name: "eye.iris", colour: Colour::rgb(0x5F, 0xD7, 0xB8) },
    ];
    let recent = [Colour::rgb(0xF2, 0x5A, 0x72), Colour::rgb(0xF5, 0x8A, 0x1F), Colour::rgb(0xB9, 0xA2, 0xFF)];
    let mut harness =
        Harness::builder().with_size(vec2(340.0, 520.0)).with_pixels_per_point(pixels_per_point).wgpu().build_ui(
            move |ui| {
                apply_settings(ui.ctx(), settings);
                let t = Theme::of(settings.theme);
                egui::Frame::new().fill(t.bg1).inner_margin(12.0).show(ui, |ui| {
                    ui.set_min_size(ui.available_size());
                    ColourField::new("Hair root colour", &mut colour)
                        .linked("hair.root")
                        .palette(&palette)
                        .recent(&recent)
                        .neighbour(PaletteSlot { name: "skin.base", colour: palette[2].colour })
                        .show(ui);
                });
            },
        );
    harness.run_steps(4);
    harness.get_by_label("Hair root colour").click();
    harness.run_steps(4);
    harness.render()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ds::tokens::Scale;

    /// The largest texture every GPU takes, per side, in pixels: a longer capture is refused at render time.
    const MAX_TEXTURE_SIDE: f32 = 8192.0;
    /// The tallest a capture part may be: half the texture limit, so a story or two can be added before a split.
    const MAX_PART_SIDE: f32 = MAX_TEXTURE_SIDE / 2.0;
    /// How many parts each section of the pseudo-locale gallery is captured in at 200%, and so the parts
    /// `tools/ci/evidence.py` runs for gallery-pseudo (crates/omuzaki-ui/evidence.toml). A section that grows past
    /// [`MAX_PART_SIDE`] needs a bigger count here and one more step there.
    const PSEUDO_PARTS: [(Group, usize); 5] = [
        (Group::Foundations, 1),
        (Group::Controls, 4),
        (Group::Display, 2),
        (Group::Feedback, 3),
        (Group::Composites, 1),
    ];

    #[test]
    fn every_part_of_the_pseudo_gallery_at_200_percent_is_under_half_a_texture() {
        let settings = Settings { scale: Scale::nearest(200), ..Settings::default() };
        for (group, parts) in PSEUDO_PARTS {
            for index in 0..parts {
                let shot = Shot { group: Some(group), part: Some((index, parts)), pseudo: true, ..Shot::new(settings) };
                // The texture is laid-out points x pixels per point x the zoom (the UI scale), as the renderer makes it.
                let pixels = content_height(shot) * shot.pixels_per_point * shot.settings.scale.factor();
                // A part that lays out to nothing would pass the limit for the wrong reason.
                assert!(pixels > 0.0, "{} part {} of {parts} laid out to nothing", group.title(), index + 1);
                assert!(
                    pixels <= MAX_PART_SIDE,
                    "{} part {} of {parts} is {pixels} px tall, over the {MAX_PART_SIDE} px part limit. Split the {} \
                     section into more parts: raise its count in PSEUDO_PARTS (crates/omuzaki-ui/src/ds/gallery/capture.rs) \
                     and add the new `--part` steps to gallery-pseudo in crates/omuzaki-ui/evidence.toml.",
                    group.title(),
                    index + 1,
                    group.title()
                );
            }
        }
    }
}
