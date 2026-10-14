//! Section: a collapsible group with a header (`design-system.md` section 7.0, `shell.md` section 7).
//!
//! A click toggles; Ctrl+click opens this section and collapses its siblings (Blender's panel gesture). Essentials
//! open by default, Advanced stays collapsed behind a count badge ("6 more").

use egui::collapsing_header::CollapsingState;
use egui::{Align2, Id, Response, Sense, Ui, WidgetInfo, WidgetType, accesskit, pos2, vec2};

use super::ids;
use crate::ds::a11y;
use crate::ds::icons;
use crate::ds::paint::{self, RingPlacement};
use crate::ds::theme::ds;
use crate::ds::tokens::{radius, size, space};

/// What showing a section returned.
pub struct SectionOutput<R> {
    /// The header's response.
    pub header: Response,
    /// The body's result, when it is showing (open or animating).
    pub body: Option<R>,
    /// Whether the section is open.
    pub open: bool,
}

/// A collapsible section.
#[must_use = "a section does nothing until it is shown"]
pub struct Section<'a> {
    title: &'a str,
    badge: Option<&'a str>,
    default_open: bool,
    group: Option<Id>,
    salt: Option<Id>,
}

/// What a section's salt is namespaced by (see [`ids`]).
const KIND: &str = "ds.section";

impl<'a> Section<'a> {
    /// A section titled `title` (also its accessible name), closed until opened.
    pub fn new(title: &'a str) -> Self {
        a11y::require_label(title);
        Self { title, badge: None, default_open: false, group: None, salt: None }
    }

    /// Starts open (Essentials).
    pub const fn default_open(mut self, open: bool) -> Self {
        self.default_open = open;
        self
    }

    /// A badge after the title, such as "6 more".
    pub const fn badge(mut self, badge: &'a str) -> Self {
        self.badge = Some(badge);
        self
    }

    /// Sections sharing a group id collapse each other on Ctrl+click.
    pub fn group(mut self, group: Id) -> Self {
        self.group = Some(group);
        self
    }

    /// Keys the section's state (open or closed, the keyboard focus on its header) by `salt` instead of by its title,
    /// so two sections both called "Advanced" in one `Ui` do not open together, and a title that is translated or
    /// reworded does not forget the state. Use a value that names the group of settings, unique among the sections
    /// of one `Ui`.
    pub fn id_salt(mut self, salt: impl std::hash::Hash + std::fmt::Debug) -> Self {
        self.salt = Some(Id::new(salt));
        self
    }

    /// The key of this section's state.
    ///
    /// The salt when there is one. Without it the title, so a section keeps its state when the widgets above it come
    /// and go; and because titles are not unique, a second section of the same title in the same `Ui` and pass is
    /// told apart by how many came before it. That makes two "Advanced" sections two sections without anyone having
    /// to remember a salt, at the price that swapping them swaps nothing: only a salt can say which is which.
    fn state_id(&self, ui: &Ui) -> Id {
        if let Some(salt) = self.salt {
            return ids::salted(ui, KIND, salt);
        }
        let titled = ui.make_persistent_id(("section", self.title));
        let ctx = ui.ctx();
        let pass = ctx.cumulative_pass_nr();
        let earlier = ctx.data_mut(|d| {
            let (seen_in, count) = d.get_temp_mut_or_insert_with(titled.with("count"), || (pass, 0u32));
            if *seen_in != pass {
                (*seen_in, *count) = (pass, 0);
            }
            let earlier = *count;
            *count += 1;
            earlier
        });
        if earlier == 0 { titled } else { titled.with(earlier) }
    }

    /// Shows the header and, when open, the body.
    pub fn show<R>(self, ui: &mut Ui, add_body: impl FnOnce(&mut Ui) -> R) -> SectionOutput<R> {
        let ds = ds(ui.ctx());
        let (t, ctx) = (&ds.theme, ui.ctx().clone());
        let id = self.state_id(ui);
        let mut state = CollapsingState::load_with_default_open(&ctx, id, self.default_open);

        // Another section in the group was Ctrl+clicked: close this one (once per click).
        if let Some(group) = self.group {
            let solo = ctx.data(|d| d.get_temp::<(Id, u64)>(group));
            let seen = ctx.data(|d| d.get_temp::<u64>(id.with("seen_solo"))).unwrap_or(0);
            if let Some((owner, nonce)) = solo
                && nonce > seen
            {
                ctx.data_mut(|d| d.insert_temp(id.with("seen_solo"), nonce));
                if owner != id {
                    state.set_open(false);
                }
            }
        }

        let (rect, response) =
            ids::allocate(ui, vec2(ui.available_width(), size::SECTION_HEADER), Sense::click(), KIND, self.salt);
        if response.clicked() {
            let ctrl = ctx.input(|i| i.modifiers.command);
            if ctrl && self.group.is_some() {
                state.set_open(true);
                if let Some(group) = self.group {
                    let nonce = ctx.data(|d| d.get_temp::<(Id, u64)>(group)).map_or(1, |(_, n)| n + 1);
                    ctx.data_mut(|d| {
                        d.insert_temp(group, (id, nonce));
                        d.insert_temp(id.with("seen_solo"), nonce);
                    });
                }
            } else {
                state.toggle(ui);
            }
        }
        let openness = state.openness(&ctx);

        if ui.is_rect_visible(rect) {
            let painter = ui.painter();
            let hover = paint::fade(&ctx, &ds, id, paint::hot(&response));
            if hover > 0.0 {
                painter.rect_filled(rect, radius::CONTROL, t.bg3.gamma_multiply(hover * 0.7));
            }
            // The chevron turns from right to down as the section opens.
            let centre = pos2(rect.left() + 10.0, rect.center().y);
            let glyph = if openness > 0.5 { icons::CARET_DOWN } else { icons::CARET_RIGHT };
            paint::icon(painter, &ds, centre, glyph, 12.0, t.text2, false);
            let title = paint::text(
                painter,
                pos2(rect.left() + 24.0, rect.center().y),
                Align2::LEFT_CENTER,
                self.title,
                &ds.text.section,
                t.text,
            );
            if let Some(badge) = self.badge {
                let width = paint::text_width(&ctx, badge, &ds.text.micro) + 2.0 * space::S6;
                let pill = egui::Rect::from_min_size(
                    pos2(title.right() + space::S8, rect.center().y - 8.0),
                    vec2(width, 16.0),
                );
                painter.rect_filled(pill, radius::FULL, t.bg3);
                paint::text(painter, pill.center(), Align2::CENTER_CENTER, badge, &ds.text.micro, t.text2);
            }
            paint::focus_ring(painter, t, &response, rect, radius::CONTROL, RingPlacement::Inside);
        }

        let open = state.is_open();
        let title = self.title;
        a11y::describe(
            &response,
            || WidgetInfo::labeled(WidgetType::CollapsingHeader, true, title),
            |node| {
                node.set_role(accesskit::Role::DisclosureTriangle);
                node.set_expanded(open);
            },
        );

        let body = state.show_body_unindented(ui, add_body).map(|inner| inner.inner);
        state.store(&ctx);
        SectionOutput { header: response, body, open }
    }
}
