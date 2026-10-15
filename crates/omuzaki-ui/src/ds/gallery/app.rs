//! The gallery window's content: a header with the appearance controls, then a card per story.
//!
//! The header uses the design system's own controls, so the gallery is also the first real consumer of them.

use std::any::Any;
use std::collections::HashMap;
use std::sync::Arc;

use egui::{Align, CentralPanel, Frame, Layout, Margin, Panel, RichText, ScrollArea, Stroke, Ui};

use super::layout::raised;
use super::story::{self, Group, Story};
use super::text::{tr, translate};
use crate::ds::icons;
use crate::ds::strings::{English, Msg, PSEUDO_LOCALE, Pseudo};
use crate::ds::theme::{Ds, Settings, apply_settings, ds, set_strings};
use crate::ds::tokens::{Density, Scale, ThemeKind, radius};
use crate::ds::widgets::{Button, Segment, Segmented, Toggle};

/// Narrowest a story card gets; the number of columns follows from the window width.
const CARD_MIN_WIDTH: f32 = 440.0;
const GAP: f32 = 16.0;
/// Until a card has been measured, columns are balanced as if every card were this tall.
const ESTIMATED_CARD_HEIGHT: f32 = 380.0;

/// The gallery's state: the appearance settings and each story's values.
pub struct Gallery {
    /// What the header edits; applied to the context every frame.
    pub settings: Settings,
    /// Speak the pseudo-locale (accented, 40% longer) instead of English: how a hard-coded string or a layout that
    /// cannot take longer text is found.
    pub pseudo: bool,
    stories: Vec<&'static Story>,
    states: HashMap<&'static str, Box<dyn Any>>,
    /// Header plus scrolled content, as last laid out: what a screenshot needs to show everything.
    content_height: f32,
    /// Each card's height from the last frame, for balancing the columns.
    heights: HashMap<&'static str, f32>,
    /// Which story went in which column last frame.
    layout: Vec<Vec<&'static str>>,
}

impl Gallery {
    /// A gallery over every registered story.
    pub fn new(settings: Settings) -> Self {
        Self {
            settings,
            pseudo: false,
            stories: story::all(),
            states: HashMap::new(),
            content_height: 0.0,
            heights: HashMap::new(),
            layout: Vec::new(),
        }
    }

    /// The same gallery, speaking the pseudo-locale.
    #[must_use]
    pub const fn with_pseudo(mut self, pseudo: bool) -> Self {
        self.pseudo = pseudo;
        self
    }

    /// Only the stories of one group (`None`: every group), for a screenshot of one section.
    #[must_use]
    pub fn with_group(mut self, group: Option<Group>) -> Self {
        if let Some(group) = group {
            self.stories.retain(|story| story.group == group);
        }
        self
    }

    /// Draws the whole gallery into `ui` (the window's root). Applies the settings and the language first.
    pub fn ui(&mut self, ui: &mut Ui) {
        apply_settings(ui.ctx(), self.settings);
        if (ds(ui.ctx()).strings.locale() == PSEUDO_LOCALE) != self.pseudo {
            set_strings(ui.ctx(), if self.pseudo { Arc::new(Pseudo(English)) } else { Arc::new(English) });
        }
        let ds = ds(ui.ctx());
        let header = Panel::top("gallery_header")
            .frame(Frame::new().fill(ds.theme.bg2).inner_margin(Margin::symmetric(16, 10)))
            .show_separator_line(false)
            .show(ui, |ui| self.header(ui, &ds));
        let header_height = header.response.rect.height();
        CentralPanel::default().frame(Frame::new().fill(ds.theme.bg0)).show(ui, |ui| {
            let scrolled = ScrollArea::vertical().auto_shrink(false).show(ui, |ui| {
                ui.add_space(GAP);
                Frame::new().inner_margin(Margin::symmetric(16, 0)).show(ui, |ui| self.body(ui, &ds));
                ui.add_space(GAP);
            });
            self.content_height = header_height + scrolled.content_size.y;
        });
    }

    /// The height the whole gallery needs, from the last frame: a screenshot sizes its window to it.
    pub fn content_height(&self) -> f32 {
        self.content_height
    }

    /// The title, then the appearance controls, laid out left to right so Tab follows the eye.
    fn header(&mut self, ui: &mut Ui, ds: &Ds) {
        let t = &ds.theme;
        ui.horizontal(|ui| {
            ui.label(RichText::new(translate(ds, "Omuzaki design system")).font(ds.text.title.clone()).color(t.text));
            ui.label(
                RichText::new(translate(ds, "omuzaki-ui · ds · gallery"))
                    .font(egui::FontId::monospace(11.5))
                    .color(t.text3),
            );
        });
        ui.add_space(6.0);
        // Wraps in a narrow window instead of running off its edge.
        ui.horizontal_wrapped(|ui| {
            let (dark, light, high) =
                (ds.text(Msg::ThemeDark), ds.text(Msg::ThemeLight), ds.text(Msg::ThemeHighContrast));
            let mut kind = self.settings.theme;
            let segments = [
                Segment::new(ThemeKind::Dark, &dark),
                Segment::new(ThemeKind::Light, &light),
                Segment::new(ThemeKind::HighContrast, &high),
            ];
            if ui.add(Segmented::new(tr!(ds, "Theme"), &mut kind, &segments)).changed() {
                self.settings.theme = kind;
            }
            ui.add_space(12.0);
            let (comfortable, compact, touch) =
                (ds.text(Msg::DensityComfortable), ds.text(Msg::DensityCompact), ds.text(Msg::DensityTouch));
            let mut density = self.settings.density;
            let segments = [
                Segment::new(Density::Comfortable, &comfortable),
                Segment::new(Density::Compact, &compact),
                Segment::new(Density::Touch, &touch),
            ];
            if ui.add(Segmented::new(tr!(ds, "Density"), &mut density, &segments)).changed() {
                self.settings.density = density;
            }
            ui.add_space(12.0);
            self.scale_controls(ui, ds);
            ui.add_space(12.0);
            let mut reduce = self.settings.reduce_motion;
            if ui.add(Toggle::switch(tr!(ds, "Reduce motion"), &mut reduce)).changed() {
                self.settings.reduce_motion = reduce;
            }
            ui.add_space(12.0);
            ui.add(Toggle::switch(tr!(ds, "Pseudo-locale"), &mut self.pseudo));
        });
    }

    fn scale_controls(&mut self, ui: &mut Ui, ds: &Ds) {
        let percent = self.settings.scale.percent();
        let index = Scale::STEPS.iter().position(|&s| s == percent).unwrap_or(2);
        if ui
            .add(
                Button::new(tr!(ds, "Smaller"))
                    .icon(icons::MINUS)
                    .ghost()
                    .disabled_if(index == 0, tr!(ds, "Smallest size")),
            )
            .clicked()
        {
            self.settings.scale = Scale::nearest(Scale::STEPS[index.saturating_sub(1)]);
        }
        ui.label(RichText::new(format!("{percent}%")).font(egui::FontId::monospace(12.0)).color(ds.theme.text2));
        if ui
            .add(
                Button::new(tr!(ds, "Larger"))
                    .icon(icons::PLUS)
                    .ghost()
                    .disabled_if(index + 1 >= Scale::STEPS.len(), tr!(ds, "Largest size")),
            )
            .clicked()
        {
            self.settings.scale = Scale::nearest(Scale::STEPS[(index + 1).min(Scale::STEPS.len() - 1)]);
        }
    }

    fn body(&mut self, ui: &mut Ui, ds: &Ds) {
        let width = ui.available_width();
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "a small positive column count")]
        let columns = (((width + GAP) / (CARD_MIN_WIDTH + GAP)).floor() as usize).max(1);
        let column_width = (width - GAP * (columns as f32 - 1.0)) / columns as f32;
        let layout = self.assign(columns);
        let ids: Vec<Vec<&'static str>> = layout.iter().map(|column| column.iter().map(|s| s.id).collect()).collect();
        if ids != self.layout {
            // Card heights were estimates: lay out again with what was just measured.
            self.layout = ids;
            ui.ctx().request_repaint();
        }
        ui.horizontal_top(|ui| {
            ui.spacing_mut().item_spacing.x = GAP;
            for column in &layout {
                ui.vertical(|ui| {
                    ui.set_width(column_width);
                    ui.spacing_mut().item_spacing.y = GAP;
                    for story in column {
                        let state = self.states.entry(story.id).or_insert_with(|| (story.new_state)());
                        let height = card(ui, ds, story, state.as_mut());
                        self.heights.insert(story.id, height);
                    }
                });
            }
        });
    }

    /// Deals the stories, in order, into whichever column is shortest so far (by last frame's measured card heights),
    /// so the page packs like masonry instead of leaving a tall column next to a short one.
    fn assign(&self, columns: usize) -> Vec<Vec<&'static Story>> {
        let mut out: Vec<Vec<&'static Story>> = vec![Vec::new(); columns];
        let mut filled = vec![0.0_f32; columns];
        for story in &self.stories {
            let shortest = filled.iter().enumerate().min_by(|a, b| a.1.total_cmp(b.1)).map_or(0, |(i, _)| i);
            filled[shortest] += self.heights.get(story.id).copied().unwrap_or(ESTIMATED_CARD_HEIGHT) + GAP;
            out[shortest].push(story);
        }
        out
    }
}

/// One story's card; returns its height so the next frame can balance the columns.
fn card(ui: &mut Ui, ds: &Ds, story: &Story, state: &mut dyn Any) -> f32 {
    raised(ds)
        .fill(ds.theme.bg1)
        .stroke(Stroke::new(1.0, ds.theme.line))
        .corner_radius(radius::CARD)
        .inner_margin(Margin::same(16))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label(RichText::new(translate(ds, story.title)).font(ds.text.section.clone()).color(ds.theme.text));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.label(
                        RichText::new(translate(ds, story.group.title()))
                            .font(ds.text.micro.clone())
                            .color(ds.theme.text3),
                    );
                });
            });
            ui.add_space(8.0);
            (story.show)(ui, state);
        })
        .response
        .rect
        .height()
}
