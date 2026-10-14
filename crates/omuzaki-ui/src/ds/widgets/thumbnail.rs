//! `ThumbnailTile` (`design-system.md` 7.0): a 4:3 preview with its label under it, for the library, the expression
//! strip and the choices of a form ("show choices as previews, not names", P2).
//!
//! The picture is **lazy**. The tile never loads anything: the host gives it a [`Thumb`] and the tile says whether it
//! is on screen and still waiting ([`TileOutput::wants_texture`]), so a grid of a thousand tiles starts the loads of
//! the dozen the person can see, and the host keeps its own cap on uploads per frame (`design-system.md` section 9,
//! rule 7). Until the texture arrives the tile shows a placeholder that has the same size, so nothing shifts when it
//! does. A picture that cannot be made says so in words, not only with a different icon.
//!
//! Hovering or focusing a tile asks for a dry-run preview of what it stands for ([`TileOutput::previewing`]), which the
//! host applies to the viewport and takes away again when it stops being true. A tile is one Tab stop and one named
//! `Button` with a selected state: the label is its name, and its description says "Loading preview", "No preview" or
//! "Previewing".

use egui::epaint::RectShape;
use egui::{
    Align2, Color32, Pos2, Rect, Response, Sense, Shape, Stroke, StrokeKind, TextureId, Ui, Widget, WidgetInfo,
    WidgetType, pos2, vec2,
};

use super::tooltip::{Tip, TipExt as _};
use crate::ds::a11y;
use crate::ds::contrast::{Rule, Table};
use crate::ds::icons;
use crate::ds::paint::{self, RingPlacement};
use crate::ds::strings::Msg;
use crate::ds::theme::{Ds, ds};
use crate::ds::tokens::{Theme, radius, space};
use crate::register_contrast;

/// The picture of a tile, as far as the host has got.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum Thumb {
    /// Not there yet: a placeholder shows, and an on-screen tile asks for the texture.
    #[default]
    Loading,
    /// The texture to draw, stretched over the preview area (the host renders it 4:3).
    Ready(TextureId),
    /// It could not be made: the placeholder says so.
    Missing,
}

/// Width of a tile unless the caller says otherwise: 96 pt, the size the atlas caches (section 9, rule 7).
pub const DEFAULT_WIDTH: f32 = 96.0;
/// Height of the label row under the preview.
const LABEL_ROW: f32 = 20.0;
/// The preview is 4:3.
const ASPECT: f32 = 0.75;

/// A preview with its label.
#[must_use = "a tile does nothing until it is added to a Ui"]
pub struct ThumbnailTile<'a> {
    label: &'a str,
    thumb: Thumb,
    selected: bool,
    width: f32,
    disabled: Option<&'a str>,
    salt: Option<egui::Id>,
}

impl<'a> ThumbnailTile<'a> {
    /// A tile named `label` showing `thumb`.
    pub fn new(label: &'a str, thumb: Thumb) -> Self {
        a11y::require_label(label);
        Self { label, thumb, selected: false, width: DEFAULT_WIDTH, disabled: None, salt: None }
    }

    /// Marks the tile as the chosen one: a 2 pt accent outline, a check mark and the strong weight.
    pub const fn selected(mut self, selected: bool) -> Self {
        self.selected = selected;
        self
    }

    /// The tile's width; the preview is 4:3 of it.
    pub const fn width(mut self, width: f32) -> Self {
        self.width = width;
        self
    }

    /// Disables the tile, saying why.
    pub const fn disabled(mut self, reason: &'a str) -> Self {
        self.disabled = Some(reason);
        self
    }

    /// Keys the tile by `salt` instead of by position.
    pub fn id_salt(mut self, salt: impl std::hash::Hash + std::fmt::Debug) -> Self {
        self.salt = Some(egui::Id::new(salt));
        self
    }

    /// Shows the tile and reports what the host needs to know.
    pub fn show(self, ui: &mut Ui) -> TileOutput {
        let ds = ds(ui.ctx());
        let t = &ds.theme;
        let enabled = ui.is_enabled() && self.disabled.is_none();
        let looks_enabled = self.disabled.is_none();
        let preview_h = (self.width * ASPECT).round();
        let (rect, outer) = ui.allocate_exact_size(vec2(self.width, preview_h + space::S4 + LABEL_ROW), Sense::hover());
        let id = self.salt.map_or(outer.id.with("tile"), |salt| ui.make_persistent_id(salt));
        let sense = if enabled { Sense::click() } else { Sense::focusable_noninteractive() };
        let mut response = ui.interact(rect, id, sense);
        let rect = paint::snap(ui.ctx(), rect);
        let preview = Rect::from_min_size(rect.min, vec2(rect.width(), preview_h));
        let on_screen = ui.is_rect_visible(rect);

        let mut cut = false;
        if on_screen {
            let hover = paint::fade(ui.ctx(), &ds, id.with("hover"), enabled && paint::hot(&response));
            let painter = ui.painter();
            paint_thumb(painter, &ds, preview, self.thumb, radius::CONTROL, looks_enabled);
            let outline = if self.selected {
                Stroke::new(2.0, paint::dim(looks_enabled, t.accent))
            } else {
                Stroke::new(t.control_stroke, paint::lerp(t.border_control, t.text3, hover * 0.6))
            };
            painter.rect_stroke(preview, radius::CONTROL, outline, StrokeKind::Inside);
            if self.selected {
                paint_check(painter, &ds, preview);
            }
            let font = if self.selected { &ds.text.strong } else { &ds.text.small };
            let ink = paint::ink(t, looks_enabled, if self.selected || hover > 0.5 { t.text } else { t.text2 });
            let label_area = Rect::from_min_max(pos2(rect.left(), preview.bottom() + space::S4), rect.max);
            let drawn = paint::text_in(painter, label_area, Align2::LEFT_CENTER, self.label, font, ink);
            cut = paint::text_width(painter.ctx(), self.label, font) > drawn.width() + 0.5;
            paint::focus_ring(painter, t, &response, preview, radius::CONTROL, RingPlacement::Outside);
        }

        let previewing = enabled && (response.hovered() || response.has_focus());
        let (thumb, selected, label) = (self.thumb, self.selected, self.label);
        a11y::describe(
            &response,
            || WidgetInfo::selected(WidgetType::Button, enabled, selected, label),
            |node| {
                let word = match (self.disabled, thumb, previewing) {
                    (Some(reason), _, _) => Some(std::borrow::Cow::Borrowed(reason)),
                    (None, Thumb::Loading, _) => Some(ds.text(Msg::ThumbLoading)),
                    (None, Thumb::Missing, _) => Some(ds.text(Msg::ThumbMissing)),
                    (None, Thumb::Ready(_), true) => Some(ds.text(Msg::ThumbPreviewing)),
                    (None, Thumb::Ready(_), false) => None,
                };
                if let Some(word) = word {
                    node.set_description(word.as_ref());
                }
            },
        );
        if cut || self.disabled.is_some() {
            let tip = Tip::new(self.label);
            response = response.tip(self.disabled.map_or(tip, |reason| tip.detail(reason)));
        }
        TileOutput { response, on_screen, previewing, wants_texture: on_screen && self.thumb == Thumb::Loading }
    }
}

impl Widget for ThumbnailTile<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        self.show(ui).response
    }
}

/// What a [`ThumbnailTile`] reports.
#[derive(Debug)]
pub struct TileOutput {
    /// The tile: `clicked()` chooses it.
    pub response: Response,
    /// The tile is inside the visible area.
    pub on_screen: bool,
    /// The pointer is over the tile or it has the keyboard focus: apply its dry-run preview.
    pub previewing: bool,
    /// The tile is on screen and has no picture yet: start loading it.
    pub wants_texture: bool,
}

/// Draws a thumbnail into `rect`: the picture, or a placeholder of the same size that says why there is none.
///
/// Shared with the step card, whose before and after pictures are not interactive.
pub(super) fn paint_thumb(painter: &egui::Painter, ds: &Ds, rect: Rect, thumb: Thumb, corner: u8, enabled: bool) {
    let t = &ds.theme;
    match thumb {
        Thumb::Ready(texture) => {
            // White is "no tint": the texture is drawn as it is.
            let shape = RectShape::filled(rect, corner, paint::dim(enabled, Color32::WHITE))
                .with_texture(texture, Rect::from_min_max(Pos2::ZERO, pos2(1.0, 1.0)));
            painter.add(Shape::Rect(shape));
        }
        Thumb::Loading | Thumb::Missing => {
            painter.rect_filled(rect, corner, paint::dim(enabled, t.bg3));
            let ink = paint::ink(t, enabled, t.text3);
            let missing = thumb == Thumb::Missing;
            let icon_y = if missing { rect.center().y - 7.0 } else { rect.center().y };
            let glyph = if missing { icons::EYE_SLASH } else { icons::IMAGE };
            paint::icon(painter, ds, pos2(rect.center().x, icon_y), glyph, 24.0, ink, false);
            if missing && rect.width() >= 64.0 {
                let word = ds.text(Msg::ThumbMissing);
                // The icon's line box is 29 pt tall and centred on `icon_y`, so it ends 14.5 pt below that point. The
                // word starts there, or the two line boxes overlap.
                let area =
                    Rect::from_min_max(pos2(rect.left() + 4.0, icon_y + 14.5), pos2(rect.right() - 4.0, rect.bottom()));
                paint::text_in(
                    painter,
                    area,
                    Align2::CENTER_TOP,
                    &word,
                    &ds.text.micro,
                    paint::ink(t, enabled, t.text2),
                );
            }
        }
    }
}

/// The check badge of a selected tile, in the preview's corner: selection is a shape as well as an outline.
fn paint_check(painter: &egui::Painter, ds: &Ds, preview: Rect) {
    let t = &ds.theme;
    let badge = paint::square(pos2(preview.right() - 11.0, preview.top() + 11.0), 16.0);
    painter.circle_filled(badge.center(), 8.0, t.accent);
    paint::check_mark(painter, badge.shrink(3.0), t.on_accent, 1.5);
}

register_contrast!("thumbnail", pairs);

/// The placeholder's icon and word on its fill, the selected outline and the check mark on the badge.
fn pairs(table: &mut Table<'_>) {
    let t: Theme = *table.theme();
    table.add("placeholder icon", t.text3, t.bg3, Rule::NonText);
    table.add("no-preview word", t.text2, t.bg3, Rule::Text);
    table.add("selected outline", t.accent, t.bg1, Rule::NonText);
    table.add("selected outline on a card", t.accent, t.bg2, Rule::NonText);
    table.add("check on its badge", t.on_accent, t.accent, Rule::NonText);
    table.add_on("label", t.text2, &[("bg1", t.bg1), ("bg2", t.bg2)], Rule::Text);
}
