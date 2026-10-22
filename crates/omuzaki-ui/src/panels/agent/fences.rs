//! Who holds what (`agent-sessions.md` section 5.3, AG-04): the fence chip in the status bar, the note on a held part
//! in the inspector, the marker on its outliner row.
//!
//! A fence binds agents only: the person is never stopped by one. So everything here informs and nothing blocks: the
//! note is inline and dismissible, never a modal, and an edit made while it shows goes through as always, with the agent
//! told through the stale-write conflict.

use egui::{Align, Frame, Label, Layout, Margin, Response, RichText, Sense, Ui, vec2};
use omuzaki_doc_api::DocPath;
use omuzaki_ops_api::{SessionId, SessionInfo, SessionState};

use super::super::text::{self, say};
use super::feed::AgentFeed;
use crate::ds::icons;
use crate::ds::theme::ds;
use crate::ds::tokens::{radius, space};
use crate::ds::widgets::Button;

/// How many holders the chip names before it counts the rest.
const CHIP_NAMES: usize = 2;

/// A live session that claims part of the document.
#[derive(Clone, Debug, PartialEq)]
pub struct FenceHolder {
    /// Which session.
    pub session: SessionId,
    /// What to call it: its client name.
    pub name: String,
    /// Its role file's name, for a sub-agent.
    pub role: Option<String>,
    /// The paths it claims.
    pub paths: Vec<DocPath>,
    /// Where it stands.
    pub state: SessionState,
}

/// Every live claim.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FenceBoard {
    /// The holders, oldest session first.
    pub holders: Vec<FenceHolder>,
}

impl FenceBoard {
    /// The live, fenced sessions among `sessions`.
    pub fn from_sessions(sessions: &[SessionInfo]) -> Self {
        let holders = sessions
            .iter()
            .filter(|s| !s.fence.is_empty() && !matches!(s.state, SessionState::Ended { .. }))
            .map(|s| FenceHolder {
                session: s.id,
                name: s.client.clone(),
                role: s.role.clone(),
                paths: s.fence.clone(),
                state: s.state.clone(),
            })
            .collect();
        Self { holders }
    }

    /// The board over what the feed knows.
    pub fn from_feed(feed: &AgentFeed) -> Self {
        Self::from_sessions(feed.sessions())
    }

    /// Who holds `path`: the holder whose claim is the deepest at or above it (a sub-agent working inside its
    /// parent's claim holds what it claimed).
    pub fn holder_of(&self, path: &DocPath) -> Option<&FenceHolder> {
        self.holders
            .iter()
            .filter_map(|h| {
                let depth = h.paths.iter().filter(|p| path.starts_with(p)).map(|p| p.segments().len()).max()?;
                Some((depth, h))
            })
            .max_by_key(|(depth, _)| *depth)
            .map(|(_, h)| h)
    }

    /// Whether no live session claims anything.
    pub fn is_empty(&self) -> bool {
        self.holders.is_empty()
    }
}

/// What a holder claims, in words: the last segment of each path ("Hair", "body").
fn claimed(holder: &FenceHolder) -> String {
    let names: Vec<&str> =
        holder.paths.iter().map(|p| p.segments().last().map_or("", String::as_str)).filter(|s| !s.is_empty()).collect();
    names.join(", ")
}

/// The status bar's chip: "designer holds hair · rigger holds body", cut with a count past two holders. Clicking it is
/// the caller's to answer (it opens the Agent panel on the first holder). Draws nothing and returns a hover-only
/// response when nobody holds anything.
pub fn fence_chip(ui: &mut Ui, board: &FenceBoard) -> Response {
    let ds = ds(ui.ctx());
    if board.is_empty() {
        let (_, response) = ui.allocate_exact_size(vec2(0.0, 0.0), Sense::hover());
        return response;
    }
    let mut parts: Vec<String> = board
        .holders
        .iter()
        .take(CHIP_NAMES)
        .map(|h| say(&ds, text::FENCE_HOLDS, &[("name", &h.name), ("what", &claimed(h))]).into_owned())
        .collect();
    if board.holders.len() > CHIP_NAMES {
        let more = (board.holders.len() - CHIP_NAMES).to_string();
        parts.push(say(&ds, text::FENCE_MORE, &[("count", &more)]).into_owned());
    }
    let line = parts.join(" \u{b7} ");
    let response = ui.add(Button::new(&line).ghost().icon(icons::LOCK));
    // Every holder by name for a screen reader, not only the ones that fit.
    let all: Vec<String> = board
        .holders
        .iter()
        .map(|h| say(&ds, text::FENCE_HOLDS, &[("name", &h.name), ("what", &claimed(h))]).into_owned())
        .collect();
    let all = all.join(", ");
    response.on_hover_text(all)
}

/// The note in the header of a part `holder` holds: "designer is working on this part. Your edit takes precedence and
/// it will be told." `dismissed` is the caller's, per session: once set, the note is not drawn. Returns the dismiss
/// button's response, or a hover-only one when nothing is drawn.
pub fn fence_note(ui: &mut Ui, holder: &FenceHolder, dismissed: &mut bool) -> Response {
    if *dismissed {
        let (_, response) = ui.allocate_exact_size(vec2(0.0, 0.0), Sense::hover());
        return response;
    }
    let ds = ds(ui.ctx());
    let t = &ds.theme;
    let line = say(&ds, text::FENCE_NOTE, &[("name", &holder.name)]).into_owned();
    let mut close = None;
    Frame::new()
        .fill(t.bg3)
        .stroke(egui::Stroke::new(1.0, t.agent))
        .corner_radius(radius::CONTROL)
        .inner_margin(Margin::same(8))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal_top(|ui| {
                ui.spacing_mut().item_spacing.x = space::S6;
                ui.label(RichText::new(icons::LOCK).font(ds.text.icon(14.0, false)).color(t.agent));
                ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                    let dismiss = say(&ds, text::FENCE_DISMISS, &[]);
                    close = Some(ui.add(Button::new(&dismiss).ghost()));
                    ui.add(Label::new(RichText::new(&line).color(t.text)).wrap());
                });
            });
        });
    let close = close.unwrap_or_else(|| ui.allocate_exact_size(vec2(0.0, 0.0), Sense::hover()).1);
    if close.clicked() {
        *dismissed = true;
    }
    close
}
