//! Comparing two History steps (#425, `shell.md` section 9: "select two rows to open a visual diff").
//!
//! With one row's details open, **Compare with…** (or Ctrl+click, or Ctrl+Enter on another row) opens a window over
//! the two states, older on the left:
//!
//! - **Before and after:** the two steps' pictures laid over each other with a split the person drags (or moves with ←
//!   and → once it has the keyboard), so the same pixel of each sits side by side.
//! - **Show what changed:** a heatmap of the pixels that differ, compared premultiplied by alpha as they composite (a
//!   colour change under no coverage is no change), over the newer picture greyed out, with the share that changed.
//! - **The parameter diff,** grouped by part: every value that differs between the two states with before and after.
//!   **Select** on a part's group selects that part in the outliner.
//!
//! Both states are worked out from the log's patches ([`super::history::state_at`]), so comparing reads the document
//! once and runs no op; nothing changes.

use egui::{Id, Key, Modifiers, Rect, RichText, Sense, TextureHandle, TextureOptions, Ui, WidgetInfo, vec2};
use omuzaki_char_api::{CharId, PartId};
use omuzaki_doc_api::{DocPath, Value};
use omuzaki_ops_api::{Grants, OpRecord, Runner, TxId};

use super::history::{HistoryLog, state_at, step_label};
use super::pictures::HistoryPictures;
use super::text::{Word, say};
use crate::ds::icons;
use crate::ds::theme::{Ds, ds};
use crate::ds::widgets::{Button, Section, Toggle};
use crate::panels::edit::EditFocus;

/// How far apart two channels may be and still count as the same: a little more than a rounding step.
const THRESHOLD: u8 = 8;
/// How far ← and → move the split.
const SPLIT_STEP: f32 = 0.05;
/// How often to look again while a picture is still being taken.
const LOOK_AGAIN: std::time::Duration = std::time::Duration::from_millis(300);

/// One value that differs between the two states.
#[derive(Clone, Debug, PartialEq)]
pub struct Change {
    /// Where it is, below its group's path (`params/cranium/half_width`).
    pub path: String,
    /// Its value in the older state; `None` where it wasn't there.
    pub before: Option<Value>,
    /// Its value in the newer state; `None` where it isn't there.
    pub after: Option<Value>,
}

/// The changes under one part, or under one other place in the document.
#[derive(Clone, Debug, PartialEq)]
pub struct Group {
    /// The group's place: `characters/kit/parts/head`, or the first two segments of a path outside every part.
    pub path: String,
    /// The character and part it belongs to, when it is a part.
    pub part: Option<(CharId, PartId)>,
    /// Its changes, by path.
    pub changes: Vec<Change>,
}

impl Group {
    /// The group's name in the list: the part's id, or its place.
    pub fn title(&self) -> &str {
        self.part.as_ref().map_or(self.path.as_str(), |(_, part)| part.as_str())
    }
}

/// Every value that differs between `before` and `after`, by path: objects are compared key by key, anything else
/// (numbers, text, lists) as a whole.
pub fn changes(before: &Value, after: &Value) -> Vec<Change> {
    let mut out = Vec::new();
    walk(&mut Vec::new(), Some(before), Some(after), &mut out);
    out
}

fn walk<'a>(at: &mut Vec<&'a str>, before: Option<&'a Value>, after: Option<&'a Value>, out: &mut Vec<Change>) {
    match (before, after) {
        (Some(Value::Object(a)), Some(Value::Object(b))) => {
            let mut keys: Vec<&String> = a.keys().chain(b.keys()).collect();
            keys.sort();
            keys.dedup();
            for key in keys {
                at.push(key);
                walk(at, a.get(key), b.get(key), out);
                at.pop();
            }
        }
        (a, b) if a != b => out.push(Change { path: at.join("/"), before: a.cloned(), after: b.cloned() }),
        _ => {}
    }
}

/// `changes` grouped by part (`characters/<c>/parts/<p>/…`), then by the first two segments of everything else, each
/// group's paths made relative to it.
pub fn grouped(changes: Vec<Change>) -> Vec<Group> {
    let mut groups: Vec<Group> = Vec::new();
    for change in changes {
        let segments: Vec<&str> = change.path.split('/').collect();
        let (path, part, rest) = match segments.as_slice() {
            ["characters", character, "parts", part, rest @ ..] => {
                let id = CharId::new(*character).ok().zip(PartId::new(*part).ok());
                (format!("characters/{character}/parts/{part}"), id, rest.join("/"))
            }
            [first, second, rest @ ..] => (format!("{first}/{second}"), None, rest.join("/")),
            _ => (change.path.clone(), None, String::new()),
        };
        let change = Change { path: rest, ..change };
        match groups.iter_mut().find(|g| g.path == path) {
            Some(group) => group.changes.push(change),
            None => groups.push(Group { path, part, changes: vec![change] }),
        }
    }
    groups
}

/// The heatmap of `before` against `after` (RGBA, the same size): the pixels that differ in `mark`, over `after` greyed
/// and dimmed; and the share of pixels that differ (0 to 1). `None` when the two are not the same size.
pub fn heat(before: &[u8], after: &[u8], mark: [u8; 3]) -> Option<(Vec<u8>, f32)> {
    if before.len() != after.len() || before.is_empty() || !before.len().is_multiple_of(4) {
        return None;
    }
    let premultiplied = |p: &[u8; 4]| {
        let alpha = u16::from(p[3]);
        let channel = |c: u8| u8::try_from(u16::from(c) * alpha / 255).unwrap_or(u8::MAX);
        [channel(p[0]), channel(p[1]), channel(p[2]), p[3]]
    };
    let (a, b) = (before.as_chunks::<4>().0, after.as_chunks::<4>().0);
    let mut out = Vec::with_capacity(after.len());
    let mut differ = 0_usize;
    for (pa, pb) in a.iter().zip(b) {
        let (x, y) = (premultiplied(pa), premultiplied(pb));
        if x.iter().zip(&y).any(|(x, y)| x.abs_diff(*y) > THRESHOLD) {
            differ += 1;
            out.extend_from_slice(&[mark[0], mark[1], mark[2], 255]);
        } else {
            let luma = (u32::from(pb[0]) * 3 + u32::from(pb[1]) * 6 + u32::from(pb[2])) / 20;
            let grey = u8::try_from(luma).unwrap_or(u8::MAX);
            out.extend_from_slice(&[grey, grey, grey, pb[3] / 2]);
        }
    }
    #[allow(clippy::cast_precision_loss, reason = "a picture's pixel count is far below 2^24")]
    Some((out, differ as f32 / a.len() as f32))
}

/// One side of the comparison.
#[derive(Clone, Debug)]
struct Side {
    tx: TxId,
    label: String,
}

/// The two steps' pictures as textures, once both are there.
enum Pictures {
    /// The studio takes no pictures, or a step has none.
    None,
    /// At least one is still being taken.
    Waiting,
    /// They can't be laid over each other: different sizes, or one won't decode.
    Unusable(String),
    Loaded {
        before: TextureHandle,
        after: TextureHandle,
        heat: TextureHandle,
        changed: f32,
        aspect: f32,
    },
}

/// The compare window's state.
pub struct Compare {
    before: Side,
    after: Side,
    groups: Result<Vec<Group>, String>,
    pictures: Pictures,
    /// Where the split is, from 0 (all after) to 1 (all before).
    split: f32,
    heatmap: bool,
}

impl std::fmt::Debug for Compare {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Compare")
            .field("before", &self.before)
            .field("after", &self.after)
            .field("split", &self.split)
            .field("heatmap", &self.heatmap)
            .finish_non_exhaustive()
    }
}

impl Compare {
    /// Compares the states right after `a` and `b` (in either order; the older is "before"), worked out from the
    /// document as it is now and the log.
    pub fn open(a: &OpRecord, b: &OpRecord, log: &HistoryLog, runner: &dyn Runner) -> Self {
        let (older, newer) = if a.seq <= b.seq { (a, b) } else { (b, a) };
        let groups = runner
            .get(&DocPath::root(), &Grants::person())
            .map(Option::unwrap_or_default)
            .map_err(|e| e.message)
            .and_then(|now| {
                let before = state_at(now.clone(), log, older.seq).map_err(|e| e.to_string())?;
                let after = state_at(now, log, newer.seq).map_err(|e| e.to_string())?;
                Ok(grouped(changes(&before, &after)))
            });
        Self {
            before: Side { tx: older.tx, label: step_label(older) },
            after: Side { tx: newer.tx, label: step_label(newer) },
            groups,
            pictures: Pictures::Waiting,
            split: 0.5,
            heatmap: false,
        }
    }

    /// The parameter diff, grouped by part.
    ///
    /// # Errors
    /// Why the two states can't be worked out: the log doesn't reach back to them whole.
    pub fn groups(&self) -> Result<&[Group], &str> {
        self.groups.as_deref().map_err(String::as_str)
    }

    /// The share of the picture that changed, once both pictures are loaded.
    pub fn changed(&self) -> Option<f32> {
        match &self.pictures {
            Pictures::Loaded { changed, .. } => Some(*changed),
            _ => None,
        }
    }

    /// Where the split is, from 0 to 1.
    pub const fn split(&self) -> f32 {
        self.split
    }

    /// Draws the window; false once the person closed it (its ✕, Close or Esc).
    pub fn show(&mut self, ctx: &egui::Context, pictures: Option<&HistoryPictures>, focus: Option<&EditFocus>) -> bool {
        self.load(ctx, pictures);
        let ds = ds(ctx);
        let mut open = true;
        let mut close = false;
        let title = say(ctx, Word::HistoryCompareTitle, &[]);
        egui::Window::new(title)
            .id(Id::new("history_compare"))
            .open(&mut open)
            .collapsible(false)
            .default_size([560.0, 640.0])
            .show(ctx, |ui| {
                self.sides(ui, &ds);
                self.picture(ui, &ds);
                ui.add_space(8.0);
                self.diff(ui, &ds, focus);
                ui.add_space(8.0);
                if ui.add(Button::new(&say(ctx, Word::HistoryCompareClose, &[])).ghost()).clicked() {
                    close = true;
                }
            });
        let escape = ctx.input_mut(|i| i.consume_key(Modifiers::NONE, Key::Escape));
        open && !close && !escape
    }

    /// Loads both pictures once both are there, and works the heatmap out.
    fn load(&mut self, ctx: &egui::Context, pictures: Option<&HistoryPictures>) {
        if !matches!(self.pictures, Pictures::Waiting) {
            return;
        }
        let Some(pictures) = pictures else {
            self.pictures = Pictures::None;
            return;
        };
        let (Some(before), Some(after)) = (pictures.png(self.before.tx), pictures.png(self.after.tx)) else {
            ctx.request_repaint_after(LOOK_AGAIN);
            return;
        };
        let decode = |png: Result<Vec<u8>, String>| {
            png.and_then(|png| image::load_from_memory(&png).map(|i| i.to_rgba8()).map_err(|e| e.to_string()))
        };
        let (before, after) = match (decode(before), decode(after)) {
            (Ok(before), Ok(after)) => (before, after),
            (Err(why), _) | (_, Err(why)) => {
                self.pictures = Pictures::Unusable(why);
                return;
            }
        };
        let mark = ds(ctx).theme.danger;
        let Some((heat_rgba, changed)) = (before.dimensions() == after.dimensions())
            .then(|| heat(before.as_raw(), after.as_raw(), [mark.r(), mark.g(), mark.b()]))
            .flatten()
        else {
            self.pictures = Pictures::Unusable(say(ctx, Word::HistoryCompareSizes, &[]));
            return;
        };
        let size = [after.width() as usize, after.height() as usize];
        #[allow(clippy::cast_precision_loss, reason = "a picture's sides are far below 2^24")]
        let aspect = after.width() as f32 / after.height().max(1) as f32;
        let texture = |name: &str, rgba: &[u8]| {
            let image = egui::ColorImage::from_rgba_unmultiplied(size, rgba);
            ctx.load_texture(format!("history-compare-{name}"), image, TextureOptions::LINEAR)
        };
        self.pictures = Pictures::Loaded {
            before: texture("before", before.as_raw()),
            after: texture("after", after.as_raw()),
            heat: texture("heat", &heat_rgba),
            changed,
            aspect,
        };
    }

    /// "Before: …" and "After: …".
    fn sides(&self, ui: &mut Ui, ds: &Ds) {
        let ctx = ui.ctx().clone();
        for (word, side) in [(Word::HistoryCompareBefore, &self.before), (Word::HistoryCompareAfter, &self.after)] {
            let line = say(&ctx, word, &[("label", &side.label)]);
            ui.add(egui::Label::new(RichText::new(line).font(ds.text.mono.clone()).color(ds.theme.text)).truncate());
        }
    }

    /// The pictures: split or heatmap, and the heatmap switch.
    fn picture(&mut self, ui: &mut Ui, ds: &Ds) {
        let ctx = ui.ctx().clone();
        let (before, after, heat, changed, aspect) = match &self.pictures {
            Pictures::Loaded { before, after, heat, changed, aspect } => {
                (before.id(), after.id(), heat.id(), *changed, *aspect)
            }
            Pictures::None => {
                ui.label(RichText::new(say(&ctx, Word::HistoryCompareNoPictures, &[])).color(ds.theme.text2));
                return;
            }
            Pictures::Waiting => {
                ui.label(RichText::new(say(&ctx, Word::HistoryCompareWaiting, &[])).color(ds.theme.text2));
                return;
            }
            Pictures::Unusable(why) => {
                ui.label(RichText::new(why).color(ds.theme.text2));
                return;
            }
        };
        let heatmap = say(&ctx, Word::HistoryCompareHeatmap, &[]);
        ui.add(Toggle::switch(&heatmap, &mut self.heatmap).id_salt("history_compare_heatmap"));
        // As large as the window allows, and never more than about a third of the screen's height.
        let tallest = (ctx.content_rect().height() * 0.36).max(120.0);
        let width = ui.available_width().min(480.0).min(tallest * aspect);
        let size = vec2(width, width / aspect.max(0.1));
        let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
        let full = Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
        let painter = ui.painter_at(rect);
        painter.rect_filled(rect, 4.0, ds.theme.bg2);
        if self.heatmap {
            painter.image(heat, rect, full, egui::Color32::WHITE);
        } else {
            let response = ui.interact(rect, Id::new("history_compare_split"), Sense::click_and_drag());
            let spoken = say(&ctx, Word::HistoryCompareSplit, &[]);
            response.widget_info(|| WidgetInfo::slider(true, f64::from(self.split * 100.0), &spoken));
            if let Some(at) = response.interact_pointer_pos() {
                self.split = ((at.x - rect.left()) / rect.width()).clamp(0.0, 1.0);
            }
            if response.has_focus() {
                // The split takes ← and → itself, so egui's arrow navigation never moves the keyboard off it.
                let filter = egui::EventFilter { horizontal_arrows: true, ..egui::EventFilter::default() };
                ui.memory_mut(|m| m.set_focus_lock_filter(response.id, filter));
                let (left, right) = ui.input_mut(|i| {
                    (i.consume_key(Modifiers::NONE, Key::ArrowLeft), i.consume_key(Modifiers::NONE, Key::ArrowRight))
                });
                if left {
                    self.split = (self.split - SPLIT_STEP).max(0.0);
                }
                if right {
                    self.split = (self.split + SPLIT_STEP).min(1.0);
                }
            }
            let x = rect.left() + rect.width() * self.split;
            painter.image(before, rect, full, egui::Color32::WHITE);
            let right = Rect::from_min_max(egui::pos2(x, rect.top()), rect.max);
            let uv = Rect::from_min_max(egui::pos2(self.split, 0.0), egui::pos2(1.0, 1.0));
            painter.image(after, right, uv, egui::Color32::WHITE);
            let accent = if response.has_focus() { ds.theme.accent } else { ds.theme.text };
            painter.line_segment([egui::pos2(x, rect.top()), egui::pos2(x, rect.bottom())], (2.0, accent));
            painter.circle_filled(egui::pos2(x, rect.center().y), 6.0, accent);
        }
        let percent = format!("{:.1}", changed * 100.0);
        let line = say(&ctx, Word::HistoryCompareChanged, &[("percent", &percent)]);
        ui.label(RichText::new(line).color(ds.theme.text2));
    }

    /// The parameter diff, a section per part with Select.
    fn diff(&self, ui: &mut Ui, ds: &Ds, focus: Option<&EditFocus>) {
        let ctx = ui.ctx().clone();
        let groups = match &self.groups {
            Ok(groups) => groups,
            Err(why) => {
                let line = say(&ctx, Word::HistoryCompareFailed, &[("why", why)]);
                ui.label(RichText::new(line).color(ds.theme.danger));
                return;
            }
        };
        if groups.is_empty() {
            ui.label(RichText::new(say(&ctx, Word::HistoryCompareSame, &[])).color(ds.theme.text2));
            return;
        }
        egui::ScrollArea::vertical().id_salt("history_compare_diff").max_height(320.0).show(ui, |ui| {
            for group in groups {
                let badge = say(&ctx, Word::HistoryCompareChanges, &[("count", &group.changes.len().to_string())]);
                Section::new(group.title())
                    .badge(&badge)
                    .default_open(true)
                    .id_salt(("history_compare", &group.path))
                    .show(ui, |ui| {
                        if let (Some((character, part)), Some(focus)) = (&group.part, focus) {
                            let select = say(&ctx, Word::HistoryCompareSelect, &[("part", part.as_str())]);
                            if ui.add(Button::new(&select).ghost().icon(icons::CURSOR)).clicked() {
                                if focus.character().as_ref() != Some(character) {
                                    focus.open(Some(character.clone()));
                                }
                                focus.select(part.clone());
                            }
                        }
                        for change in &group.changes {
                            let line = format!(
                                "{}: {} \u{2192} {}",
                                change.path,
                                shown(change.before.as_ref()),
                                shown(change.after.as_ref())
                            );
                            ui.add(
                                egui::Label::new(RichText::new(line).font(ds.text.mono.clone()).color(ds.theme.text2))
                                    .wrap(),
                            );
                        }
                    });
            }
        });
    }
}

/// A value as the diff shows it: compact JSON with numbers to three places, cut to 48 characters; "∅" when absent.
fn shown(value: Option<&Value>) -> String {
    let Some(value) = value else { return "\u{2205}".to_owned() };
    let text = match value {
        Value::Number(n) => n.as_f64().map_or_else(|| n.to_string(), |f| format!("{f:.3}")),
        other => other.to_string(),
    };
    if text.chars().count() > 48 { format!("{}\u{2026}", text.chars().take(47).collect::<String>()) } else { text }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn changes_walk_objects_and_compare_the_rest_whole() {
        let before =
            json!({"characters": {"kit": {"parts": {"head": {"params": {"w": 0.5, "h": 1.0}}}, "name": "Kit"}}});
        let after = json!({"characters": {"kit": {"parts": {"head": {"params": {"w": 0.6, "h": 1.0}, "tags": [1]}}, "name": "Kit"}}});
        let found = changes(&before, &after);
        assert_eq!(found.len(), 2);
        assert_eq!(found[0].path, "characters/kit/parts/head/params/w");
        assert_eq!((found[0].before.clone(), found[0].after.clone()), (Some(json!(0.5)), Some(json!(0.6))));
        assert_eq!(found[1].path, "characters/kit/parts/head/tags");
        assert_eq!(found[1].before, None);
    }

    #[test]
    fn groups_are_parts_then_places() {
        let before =
            json!({"characters": {"kit": {"parts": {"head": {"w": 1}, "hair": {"c": "red"}}, "face": {"p": "egg"}}}});
        let after = json!({"characters": {"kit": {"parts": {"head": {"w": 2}, "hair": {"c": "blue"}}, "face": {"p": "round"}}}});
        let groups = grouped(changes(&before, &after));
        let titles: Vec<&str> = groups.iter().map(Group::title).collect();
        assert_eq!(titles, ["characters/kit", "hair", "head"]);
        assert_eq!(groups[2].changes[0].path, "w");
        assert_eq!(groups[2].part.as_ref().map(|(c, p)| (c.as_str(), p.as_str())), Some(("kit", "head")));
        assert_eq!(groups[0].changes[0].path, "face/p");
    }

    #[test]
    fn the_heatmap_marks_what_changed_and_counts_it() {
        let before = [0, 0, 0, 255, 10, 10, 10, 255, 200, 0, 0, 0, 1, 2, 3, 255];
        // The third pixel changes colour under no coverage: no change.
        let after = [0, 0, 0, 255, 200, 10, 10, 255, 0, 200, 0, 0, 1, 2, 3, 255];
        let (heat, changed) = heat(&before, &after, [255, 0, 0]).unwrap();
        assert!((changed - 0.25).abs() < 1e-6, "{changed}");
        assert_eq!(&heat[4..8], &[255, 0, 0, 255]);
        assert_ne!(&heat[0..4], &[255, 0, 0, 255]);
        assert!(super::heat(&before, &after[..8], [255, 0, 0]).is_none(), "different sizes");
    }
}
