//! Helpers for testing design-system widgets with `egui_kittest`: a harness with the theme applied, an AccessKit tree
//! walker and a few input shortcuts. Available to this crate's tests and, with the `capture` feature, to the panels'
//! tests that build on the components.

use egui::{Event, Key, Modifiers, Pos2, Ui, Vec2, accesskit, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::NodeT as _;

use super::theme::{Settings, apply_settings};
use super::tokens::ThemeKind;

/// A harness whose UI is drawn with the design-system theme applied, at a panel-ish size.
pub fn harness<'a>(settings: Settings, size: Vec2, mut app: impl FnMut(&mut Ui) + 'a) -> Harness<'a> {
    let mut harness = Harness::builder().with_size(size).build_ui(move |ui| {
        apply_settings(ui.ctx(), settings);
        egui::Frame::new().fill(super::tokens::Theme::of(settings.theme).bg1).inner_margin(8.0).show(ui, |ui| app(ui));
    });
    // The fonts become active one pass after they are installed.
    harness.run_steps(3);
    harness
}

/// Like [`harness`], with a state the test can read after running frames (`harness.state()`).
pub fn harness_state<'a, S: 'a>(
    settings: Settings,
    size: Vec2,
    state: S,
    mut app: impl FnMut(&mut Ui, &mut S) + 'a,
) -> Harness<'a, S> {
    let mut harness = Harness::builder().with_size(size).build_ui_state(
        move |ui, state| {
            apply_settings(ui.ctx(), settings);
            egui::Frame::new()
                .fill(super::tokens::Theme::of(settings.theme).bg1)
                .inner_margin(8.0)
                .show(ui, |ui| app(ui, state));
        },
        state,
    );
    harness.run_steps(3);
    harness
}

/// A 360 x 420 harness in the dark theme: the size of an inspector column.
pub fn panel_harness<'a>(app: impl FnMut(&mut Ui) + 'a) -> Harness<'a> {
    harness(Settings { theme: ThemeKind::Dark, ..Settings::default() }, vec2(360.0, 420.0), app)
}

/// Presses and releases a key, with modifiers, and runs until the UI is idle.
pub fn press<S>(harness: &mut Harness<'_, S>, modifiers: Modifiers, key: Key) {
    queue_key(harness, modifiers, key);
    harness.run();
}

/// Presses and releases a key, then runs `frames` frames: for a UI that never goes idle because something in it
/// (the gallery's agent glow) asks for a repaint every frame, where [`press`] would give up waiting.
pub fn press_for<S>(harness: &mut Harness<'_, S>, modifiers: Modifiers, key: Key, frames: usize) {
    queue_key(harness, modifiers, key);
    harness.run_steps(frames);
}

fn queue_key<S>(harness: &Harness<'_, S>, modifiers: Modifiers, key: Key) {
    for pressed in [true, false] {
        harness
            .event_modifiers(Event::Key { key, physical_key: Some(key), pressed, repeat: false, modifiers }, modifiers);
    }
}

/// Moves the pointer, presses, moves along `path`, releases: one drag gesture, running until idle after each step.
pub fn drag<S>(harness: &mut Harness<'_, S>, path: &[Pos2]) {
    drag_with(harness, path, |h| {
        h.run();
    });
}

/// [`drag`] with `frames` frames after each step, for a UI that never goes idle (see [`press_for`]).
pub fn drag_for<S>(harness: &mut Harness<'_, S>, path: &[Pos2], frames: usize) {
    drag_with(harness, path, |h| h.run_steps(frames));
}

fn drag_with<S>(harness: &mut Harness<'_, S>, path: &[Pos2], settle: impl Fn(&mut Harness<'_, S>)) {
    let Some(&start) = path.first() else { return };
    harness.hover_at(start);
    settle(harness);
    harness.event(Event::PointerButton {
        pos: start,
        button: egui::PointerButton::Primary,
        pressed: true,
        modifiers: Modifiers::NONE,
    });
    settle(harness);
    for &p in &path[1..] {
        harness.event(Event::PointerMoved(p));
        settle(harness);
    }
    let end = *path.last().unwrap_or(&start);
    harness.event(Event::PointerButton {
        pos: end,
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: Modifiers::NONE,
    });
    settle(harness);
}

/// Every AccessKit node a person can operate that has no accessible name.
///
/// "Operate" means the node is interactive by role, or offers Focus or Click: a custom widget that senses clicks
/// without describing itself shows up as a nameless `Unknown` node, which is exactly the one a screen reader would
/// read out as nothing. Containers that take no action (the window, groups) are not operated and need no name.
pub fn unlabelled_interactive<S>(harness: &Harness<'_, S>) -> Vec<String> {
    unlabelled_nodes(harness).into_iter().map(|(role, rect)| format!("{role:?} at {rect:?} has no label")).collect()
}

/// The nodes [`unlabelled_interactive`] reports, by role and where they are, for a caller that says which part of the
/// window each is in (the shell names the panel).
pub fn unlabelled_nodes<S>(harness: &Harness<'_, S>) -> Vec<(accesskit::Role, egui::Rect)> {
    use accesskit::{Action, Role};
    let interactive_role = |role: Role| {
        matches!(
            role,
            Role::Button
                | Role::CheckBox
                | Role::Switch
                | Role::RadioButton
                | Role::Slider
                | Role::SpinButton
                | Role::TextInput
                | Role::ColorWell
                | Role::ComboBox
                | Role::TreeItem
                | Role::Link
                | Role::DisclosureTriangle
        )
    };
    let mut bad = Vec::new();
    for node in harness.root().children_recursive() {
        let ak = node.accesskit_node();
        // egui's own scroll bars are the toolkit's: the platform announces them as scroll bars.
        let operated = ak.role() != Role::ScrollBar
            && (interactive_role(ak.role())
                || ak.data().supports_action(Action::Focus)
                || ak.data().supports_action(Action::Click));
        let named = ak.label().is_some_and(|l| !l.trim().is_empty());
        if operated && !named {
            bad.push((ak.role(), node.rect()));
        }
    }
    bad
}

/// One stop of the Tab order.
#[derive(Clone, Debug)]
pub struct TabStop {
    /// What the node is.
    pub role: accesskit::Role,
    /// Its accessible name.
    pub label: String,
    /// Where it is on screen.
    pub rect: egui::Rect,
    /// Something is drawn on it that says it has the keyboard focus (see [`super::audit::focus_indicators`]).
    pub indicated: bool,
    /// Its order is the order of the data it edits, not of the screen: the points of a curve are visited by input
    /// value, left to right, though a rising curve climbs the screen. Marked by the component with
    /// [`ORDERED_BY_DATA`](super::a11y::ORDERED_BY_DATA) on its node.
    pub ordered_by_data: bool,
}

/// Presses Tab until the focus comes back to where it started (or `limit` presses), and records each stop.
pub fn tab_through<S>(harness: &mut Harness<'_, S>, limit: usize) -> Vec<TabStop> {
    use super::audit::focus_indicators;
    let focused = |harness: &Harness<'_, S>| {
        harness.root().children_recursive().find(|n| n.accesskit_node().is_focused()).map(|n| {
            let ak = n.accesskit_node();
            (ak.role(), ak.label().unwrap_or_default(), n.rect(), ak.class_name() == Some(super::a11y::ORDERED_BY_DATA))
        })
    };
    let theme = super::theme::ds(&harness.ctx).theme;
    let mut stops: Vec<TabStop> = Vec::new();
    for _ in 0..limit {
        press_for(harness, Modifiers::NONE, Key::Tab, 3);
        let Some((role, label, rect, ordered_by_data)) = focused(harness) else { continue };
        if stops.first().is_some_and(|first| first.rect == rect && first.label == label) {
            break;
        }
        let indicated = focus_indicators(harness.output(), &theme).iter().any(|ring| ring.intersects(rect));
        stops.push(TabStop { role, label, rect, indicated, ordered_by_data });
    }
    stops
}

/// The places where Tab does not follow the eye: a stop that is above the previous one, or to its left in the same
/// row. Rows are told apart by the vertical distance between centres, so a taller widget beside a short one still
/// counts as one row. A stop inside the previous one is in order: the group comes first, then what it holds.
pub fn reading_order_violations(stops: &[TabStop]) -> Vec<String> {
    const SAME_ROW: f32 = 12.0;
    stops
        .windows(2)
        .filter_map(|pair| {
            let (a, b) = (&pair[0], &pair[1]);
            if a.ordered_by_data && b.ordered_by_data {
                return None;
            }
            // A group that takes the focus itself (a tab strip) comes before the items inside it.
            if a.rect.expand(0.5).contains_rect(b.rect) {
                return None;
            }
            let dy = b.rect.center().y - a.rect.center().y;
            let in_order = if dy.abs() < SAME_ROW { b.rect.center().x >= a.rect.center().x - 1.0 } else { dy > 0.0 };
            (!in_order).then(|| {
                format!(
                    "{:?} '{}' at ({:.0}, {:.0}) is followed by {:?} '{}' at ({:.0}, {:.0})",
                    a.role,
                    a.label,
                    a.rect.left(),
                    a.rect.top(),
                    b.role,
                    b.label,
                    b.rect.left(),
                    b.rect.top()
                )
            })
        })
        .collect()
}

/// Every string a screen reader could be given by the tree: the label, value, description and placeholder of every
/// node.
pub fn accesskit_strings<S>(harness: &Harness<'_, S>) -> Vec<String> {
    let mut out = Vec::new();
    for node in harness.root().children_recursive() {
        let ak = node.accesskit_node();
        out.extend(ak.label());
        out.extend(ak.value());
        out.extend(ak.description());
        out.extend(ak.placeholder().map(str::to_owned));
    }
    out
}

/// Every string a frame shows or tells a screen reader: the painted text and the tree's strings.
pub fn all_strings<S>(harness: &Harness<'_, S>) -> Vec<String> {
    let mut out: Vec<String> = super::audit::painted(harness.output()).into_iter().map(|p| p.text).collect();
    out.extend(accesskit_strings(harness));
    out
}
