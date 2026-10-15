//! The UI scale audit (SE-02, `design-system.md` sections 3 and 10): every gallery story, drawn in its card at 75, 100,
//! 125, 150 and 200% UI scale, must have no text clipped, no text or control outside the window, and no two pieces of
//! text or controls drawn over one another (nesting excepted). Each failure names its story and its scale.
//!
//! The audit reads the frame egui produced (`ds::audit::layout_findings`) and the accessibility tree
//! (`control_overlaps`, `control_bounds`), so a component that draws out of place is caught without anybody listing
//! its geometry.

use std::any::Any;

use egui::accesskit::{Action, Role};
use egui::{Frame, Label, Margin, Rect, Stroke, TextWrapMode, Ui, pos2, vec2};
use egui_kittest::kittest::NodeT as _;
use egui_kittest::{Harness, Node};
use omuzaki_ui::ds::audit::{Control, LayoutFinding, LayoutProblem, control_bounds, control_overlaps, layout_findings};
use omuzaki_ui::ds::gallery::{Story, all};
use omuzaki_ui::ds::tokens::radius;
use omuzaki_ui::ds::{Scale, Settings, Theme, ThemeKind, apply_settings};

/// The UI scales the audit draws: the ones WCAG 1.4.4 and the design's scale steps make the floor and the ceiling of.
const SCALES: [u16; 5] = [75, 100, 125, 150, 200];

/// A 1680 pt wide window (the gallery's default), tall enough that no story scrolls.
const WINDOW: (f32, f32) = (1680.0, 4000.0);

/// One known layout problem, named exactly. It is expected to appear at every scale in [`SCALES`] and nowhere else:
/// a matching finding is allowed, any other finding fails, and an entry that no longer matches fails too, so it is
/// removed the moment its fix lands.
struct Expected {
    /// The story it is in.
    story: &'static str,
    /// What is wrong.
    problem: LayoutProblem,
    /// The text, or the control's name, as the finding reports it.
    what: &'static str,
    /// The other piece it is drawn over, for an overlap.
    with: Option<&'static str>,
    /// The defect, in words, until it is fixed.
    defect: &'static str,
}

impl Expected {
    fn matches(&self, story: &str, finding: &LayoutFinding) -> bool {
        self.story == story
            && self.problem == finding.problem
            && self.what == finding.what
            && self.with == finding.with.as_deref()
    }
}

/// The known failures, each named exactly. Every entry is expected at every scale in [`SCALES`]: a matching finding
/// is allowed, any other finding fails, and an entry that stops appearing fails too, so it is removed the moment its
/// fix lands. Entries with the same text and overlap are one entry, since they are one defect.
const EXPECTED_FAILURES: &[Expected] = &[
    Expected {
        story: "approval",
        problem: LayoutProblem::Overlap,
        what: "~40 s",
        with: Some("reversible: yes"),
        defect: "the approval card's chips and warning glyph overlap its reversibility line",
    },
    Expected {
        story: "approval",
        problem: LayoutProblem::Overlap,
        what: " · ",
        with: Some("reversible: yes"),
        defect: "the approval card's chips and warning glyph overlap its reversibility line",
    },
    Expected {
        story: "approval",
        problem: LayoutProblem::Overlap,
        what: "1.2 GB GPU",
        with: Some("reversible: yes"),
        defect: "the approval card's chips and warning glyph overlap its reversibility line",
    },
    Expected {
        story: "approval",
        problem: LayoutProblem::Overlap,
        what: "$0",
        with: Some("reversible: yes"),
        defect: "the approval card's chips and warning glyph overlap its reversibility line",
    },
    Expected {
        story: "approval",
        problem: LayoutProblem::Overlap,
        what: "\u{e4e0}",
        with: Some("Can't be undone"),
        defect: "the approval card's chips and warning glyph overlap its reversibility line",
    },
];

/// Draws `draw` in a gallery-style card at `scale` and returns every layout problem the frame has.
fn audit(scale: u16, draw: impl FnMut(&mut Ui) + 'static) -> Vec<LayoutFinding> {
    let settings = Settings { scale: Scale::nearest(scale), ..Settings::default() };
    let mut draw = draw;
    let mut harness = Harness::builder().with_size(vec2(WINDOW.0, WINDOW.1)).build_ui(move |ui| {
        apply_settings(ui.ctx(), settings);
        card(ui, &mut draw);
    });
    harness.run_steps(4);
    let window = harness.ctx.content_rect();
    let controls = controls(&harness);
    let mut found = layout_findings(harness.output(), window);
    found.extend(control_overlaps(&controls));
    found.extend(control_bounds(&controls, window));
    found
}

/// A card on the dark surface, as the gallery draws one: the story sits inside it, full width.
fn card(ui: &mut Ui, draw: &mut impl FnMut(&mut Ui)) {
    let t = Theme::of(ThemeKind::Dark);
    Frame::new()
        .fill(t.bg1)
        .stroke(Stroke::new(1.0, t.line))
        .corner_radius(radius::CARD)
        .inner_margin(Margin::same(16))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            draw(ui);
        });
}

/// Every node a person can operate, in tree order (parents first), with its name, rectangle and nearest operable
/// ancestor. The rule is the accessibility lint's in `ds::testing`: an interactive role, or Focus or Click.
fn controls<S>(harness: &Harness<'_, S>) -> Vec<Control> {
    let mut out = Vec::new();
    for child in harness.root().children() {
        collect(child, None, &mut out);
    }
    out
}

fn collect(node: Node<'_>, parent: Option<usize>, out: &mut Vec<Control>) {
    let mut operable_parent = parent;
    if operable(&node) {
        out.push(Control {
            name: node.accesskit_node().label().unwrap_or_default(),
            role: node.accesskit_node().role(),
            rect: node.rect(),
            parent,
        });
        operable_parent = Some(out.len() - 1);
    }
    for child in node.children() {
        collect(child, operable_parent, out);
    }
}

fn operable(node: &Node<'_>) -> bool {
    let ak = node.accesskit_node();
    let interactive = matches!(
        ak.role(),
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
    );
    ak.role() != Role::ScrollBar
        && (interactive || ak.data().supports_action(Action::Focus) || ak.data().supports_action(Action::Click))
}

#[test]
fn every_gallery_story_is_clear_of_its_window_and_itself_at_every_scale() {
    let mut problems = Vec::new();
    // (index into EXPECTED_FAILURES, scale) for every expected entry that a finding matched.
    let mut matched: Vec<(usize, u16)> = Vec::new();
    for scale in SCALES {
        for story in all() {
            for finding in audit_story(story, scale) {
                let entries: Vec<usize> = EXPECTED_FAILURES
                    .iter()
                    .enumerate()
                    .filter(|(_, e)| e.matches(story.id, &finding))
                    .map(|(index, _)| index)
                    .collect();
                if entries.is_empty() {
                    problems.push(format!("{} @ {scale}%: {finding}", story.id));
                }
                matched.extend(entries.into_iter().map(|index| (index, scale)));
            }
        }
    }
    assert!(problems.is_empty(), "{} layout problems:\n{}", problems.len(), problems.join("\n"));
    for (index, expected) in EXPECTED_FAILURES.iter().enumerate() {
        for scale in SCALES {
            assert!(
                matched.contains(&(index, scale)),
                "{} ({}) no longer shows its expected failure at {scale}%; remove its entry",
                expected.story,
                expected.defect
            );
        }
    }
}

/// One story in its card at `scale`. Its state is made fresh, as the gallery makes it.
fn audit_story(story: &'static Story, scale: u16) -> Vec<LayoutFinding> {
    let mut state: Box<dyn Any> = (story.new_state)();
    audit(scale, move |ui| (story.show)(ui, &mut *state))
}

/// A label drawn in a fixed-width box and never wrapped. Its text is wider than the layout left at 200% (the window's
/// points are halved by the zoom), so it runs past the window's edge there, and it fits at 100%.
const PLANTED_TEXT: &str = "A fixed-width label that is far longer than the space the card leaves it, so at 200% it runs \
     off the edge of the window and the audit has to catch it";

#[test]
fn a_planted_fixed_width_label_that_overflows_at_200_fails_and_is_named() {
    let planted = |ui: &mut Ui| {
        ui.add_sized(vec2(900.0, 28.0), Label::new(PLANTED_TEXT).wrap_mode(TextWrapMode::Extend));
    };
    let at_100 = audit(100, planted);
    assert!(at_100.is_empty(), "the planted fixed-width label should fit at 100%: {at_100:?}");
    let at_200 = audit(200, planted);
    assert!(
        at_200.iter().any(|f| f.problem == LayoutProblem::OutsideWindow),
        "the planted fixed-width label should fail at 200% (named in the test): {at_200:?}"
    );
}

/// A control with no children, for the synthetic cases below.
fn leaf(name: &str, rect: Rect, parent: Option<usize>) -> Control {
    Control { name: name.to_owned(), role: Role::Unknown, rect, parent }
}

fn square(x: f32, y: f32) -> Rect {
    Rect::from_min_size(pos2(x, y), vec2(40.0, 40.0))
}

#[test]
fn two_coincident_sibling_controls_are_a_collision() {
    let controls = [leaf("first", square(0.0, 0.0), None), leaf("second", square(0.0, 0.0), None)];
    let found = control_overlaps(&controls);
    assert_eq!(found.len(), 1, "identical rectangles must be reported: {found:?}");
}

#[test]
fn a_sibling_inside_another_sibling_is_a_collision() {
    let outer = Rect::from_min_size(pos2(0.0, 0.0), vec2(200.0, 40.0));
    let controls = [leaf("outer", outer, None), leaf("inner", square(10.0, 0.0), None)];
    let found = control_overlaps(&controls);
    assert_eq!(found.len(), 1, "a sibling inside a sibling must be reported: {found:?}");
}

#[test]
fn a_child_inside_its_parent_is_nesting_not_a_collision() {
    let controls = [
        leaf("parent", Rect::from_min_size(pos2(0.0, 0.0), vec2(200.0, 40.0)), None),
        leaf("child", square(10.0, 0.0), Some(0)),
    ];
    assert!(control_overlaps(&controls).is_empty(), "a child inside its parent is nesting");
}

#[test]
fn an_off_screen_parent_is_flagged_even_when_its_child_is_visible() {
    let window = Rect::from_min_size(pos2(0.0, 0.0), vec2(1000.0, 1000.0));
    let controls = [
        leaf("parent", Rect::from_min_size(pos2(1200.0, 0.0), vec2(300.0, 40.0)), None),
        leaf("child", square(900.0, 0.0), Some(0)),
    ];
    let found = control_bounds(&controls, window);
    assert_eq!(found.len(), 1, "only the parent is outside the window: {found:?}");
    assert_eq!(found[0].what, "parent");
    assert_eq!(found[0].problem, LayoutProblem::OutsideWindow);
}

/// A tab and its close button, as the tab bar exposes them: siblings in the tree, the button inside the tab's rectangle.
fn tab_and_close(close: Rect) -> [Control; 2] {
    let tab = Rect::from_min_size(pos2(0.0, 0.0), vec2(100.0, 28.0));
    [
        Control { name: "Library".to_owned(), role: Role::Tab, rect: tab, parent: None },
        Control { name: "Close Library".to_owned(), role: Role::Button, rect: close, parent: None },
    ]
}

#[test]
fn a_tabs_close_button_inside_the_tab_is_its_affordance_not_a_collision() {
    let inside = Rect::from_min_size(pos2(74.0, 2.0), vec2(24.0, 24.0));
    assert!(control_overlaps(&tab_and_close(inside)).is_empty());
}

#[test]
fn a_close_button_that_runs_out_of_its_tab_is_a_collision() {
    let spills = Rect::from_min_size(pos2(90.0, 2.0), vec2(24.0, 24.0));
    assert_eq!(control_overlaps(&tab_and_close(spills)).len(), 1);
}

#[test]
fn a_button_inside_something_that_is_not_a_tab_is_still_a_collision() {
    let mut controls = tab_and_close(Rect::from_min_size(pos2(74.0, 2.0), vec2(24.0, 24.0)));
    controls[0].role = Role::Button;
    assert_eq!(control_overlaps(&controls).len(), 1);
}
