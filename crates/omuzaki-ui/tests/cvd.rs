//! Colour-blind-safe themes (SE-02, `design-system.md` sections 2.3 and 10): in every theme the colours that mean
//! different things stay apart under protan, deutan and tritan vision, and no state is signalled by colour alone.
//!
//! The first is a numeric check on the tokens ([`omuzaki_ui::ds::contrast::separations`]). The second reads the frame
//! egui produced for every gallery story and finds each shape painted in a status colour that has no icon or word
//! beside it, so a component that forgets the cue is caught the first time the gallery shows it.

use egui::epaint::{ColorMode, PathStroke};
use egui::{Color32, FullOutput, Rect, Shape, Stroke, Ui, Vec2, vec2};
use omuzaki_ui::ds::audit::painted;
use omuzaki_ui::ds::contrast::{MIN_CVD_DELTA_E, Separation, separations};
use omuzaki_ui::ds::gallery::all;
use omuzaki_ui::ds::testing::harness;
use omuzaki_ui::ds::tokens::{DARK, HIGH_CONTRAST, LIGHT};
use omuzaki_ui::ds::{English, Msg, Settings, Status, Strings, Theme, ThemeKind, icons};

fn failing(theme: &Theme) -> Vec<String> {
    separations(theme)
        .iter()
        .filter(|s| !s.passes())
        .map(|s| {
            let (d, de) = s.worst();
            format!("{:?}: {} is {de:.1} under {} vision, needs {MIN_CVD_DELTA_E}", theme.kind, s.name, d.as_str())
        })
        .collect()
}

#[test]
fn every_theme_keeps_its_meaningful_colours_apart_under_every_deficiency() {
    let failures: Vec<String> = [&DARK, &LIGHT, &HIGH_CONTRAST].into_iter().flat_map(failing).collect();
    assert!(failures.is_empty(), "colour pairs that look alike:\n{}", failures.join("\n"));
}

/// The table covers what the issue names, in every theme: all status pairs, agent/person/accent, the meter ends.
#[test]
fn the_check_covers_status_identity_and_meter_pairs() {
    for theme in [&DARK, &LIGHT, &HIGH_CONTRAST] {
        let names: Vec<String> = separations(theme).into_iter().map(|s| s.name).collect();
        for want in
            ["ok vs danger", "warn vs danger", "info vs ok", "person vs agent", "agent vs accent", "accent vs warn"]
        {
            assert!(names.iter().any(|n| n == want), "{:?}: no pair '{want}' in {names:?}", theme.kind);
        }
        assert!(names.iter().any(|n| n.starts_with("meter dimmest fill")), "{:?}: no meter pair", theme.kind);
    }
}

/// Red and green of equal lightness collapse for deutan vision: the check must refuse them.
#[test]
fn a_planted_red_green_status_pair_of_equal_lightness_fails() {
    let planted =
        Theme { ok: Color32::from_rgb(0x30, 0x90, 0x30), danger: Color32::from_rgb(0xC0, 0x40, 0x40), ..DARK };
    let failures = failing(&planted);
    assert!(failures.iter().any(|f| f.contains("ok vs danger") && f.contains("deuteranopia")), "{failures:?}");
    let pair = Separation { name: "planted".into(), a: planted.ok, b: planted.danger };
    assert!(!pair.passes());
}

const CARD: Vec2 = vec2(1200.0, 3000.0);
/// How far from a status-coloured shape its icon or word still counts as its cue, in points.
const CUE_REACH: f32 = 32.0;
/// Smaller shapes (hairlines, caret ticks) are not signals by themselves.
const MIN_MARK: f32 = 4.0;

const STATUSES: [Status; 3] = [Status::Ok, Status::Warn, Status::Danger];

/// Flattens nested shapes into paint order.
fn walk<'a>(shape: &'a Shape, out: &mut Vec<&'a Shape>) {
    match shape {
        Shape::Vec(inner) => inner.iter().for_each(|s| walk(s, out)),
        other => out.push(other),
    }
}

/// The status whose colour is exactly `colour` (opaque), if any. Tints and blends are not signals.
fn status_of(colour: Color32, theme: &Theme) -> Option<Status> {
    let same = |s: Color32| (colour.r(), colour.g(), colour.b()) == (s.r(), s.g(), s.b());
    STATUSES.into_iter().find(|status| colour.a() == 255 && same(theme.status(*status)))
}

/// The status colours a painted primitive uses, with the area it covers. Every primitive that can carry a colour is
/// read, so a bare path or mesh cannot slip past; text is a cue, not a mark, and is skipped.
fn marks(shape: &Shape, theme: &Theme) -> Vec<(Status, Rect)> {
    let path_colour = |stroke: &PathStroke| match &stroke.color {
        ColorMode::Solid(c) => *c,
        ColorMode::UV(_) => Color32::TRANSPARENT,
    };
    // `thin` shapes (lines, paths) are judged by their longest side, filled areas by their shortest.
    let (rect, colours, thin): (Rect, Vec<Color32>, bool) = match shape {
        Shape::Rect(r) => (r.rect, vec![r.fill, r.stroke.color], false),
        Shape::Circle(c) => {
            (Rect::from_center_size(c.center, Vec2::splat(c.radius * 2.0)), vec![c.fill, c.stroke.color], false)
        }
        Shape::Ellipse(e) => (Rect::from_center_size(e.center, e.radius * 2.0), vec![e.fill, e.stroke.color], false),
        Shape::LineSegment { points, stroke } => (Rect::from_two_pos(points[0], points[1]), vec![stroke.color], true),
        Shape::Path(p) => (Rect::from_points(&p.points), vec![p.fill, path_colour(&p.stroke)], true),
        Shape::QuadraticBezier(b) => (Rect::from_points(&b.points), vec![b.fill, path_colour(&b.stroke)], true),
        Shape::CubicBezier(b) => (Rect::from_points(&b.points), vec![b.fill, path_colour(&b.stroke)], true),
        Shape::Mesh(m) => (m.calc_bounds(), m.vertices.iter().map(|v| v.color).collect(), false),
        _ => return Vec::new(),
    };
    let size = if thin { rect.width().max(rect.height()) } else { rect.width().min(rect.height()) };
    if size < MIN_MARK {
        return Vec::new();
    }
    let mut found: Vec<Status> = colours.into_iter().filter_map(|c| status_of(c, theme)).collect();
    found.dedup();
    found.into_iter().map(|status| (status, rect)).collect()
}

/// Whether `text` is the cue of `status`: its own icon, or its word ("OK", "Warning", "Error") as a whole word, so
/// "book" is not "OK". An unrelated label beside or inside the shape says nothing about what the colour means.
fn is_cue(text: &str, status: Status) -> bool {
    let word = match status {
        Status::Ok => Msg::StatusOk,
        Status::Warn => Msg::StatusWarning,
        Status::Danger => Msg::StatusError,
        Status::Info => Msg::StatusInfo,
    };
    let word = English.pattern(word).to_lowercase();
    text.contains(icons::status(status)) || text.to_lowercase().split(|c: char| !c.is_alphanumeric()).any(|w| w == word)
}

/// A shape painted in a status colour with no icon or word of that status within [`CUE_REACH`].
#[derive(Clone, Copy, Debug)]
struct Bare {
    status: Status,
    rect: Rect,
}

fn colour_alone(output: &FullOutput, theme: &Theme) -> Vec<Bare> {
    let mut flat: Vec<&Shape> = Vec::new();
    for clipped in &output.shapes {
        walk(&clipped.shape, &mut flat);
    }
    let cues: Vec<(String, Rect)> = painted(output).into_iter().map(|p| (p.text, p.rect)).collect();
    let mut out = Vec::new();
    for shape in flat {
        for (status, rect) in marks(shape, theme) {
            let near = rect.expand(CUE_REACH);
            if !cues.iter().any(|(text, at)| at.intersects(near) && is_cue(text, status)) {
                out.push(Bare { status, rect });
            }
        }
    }
    out
}

fn frame_of(theme: ThemeKind, draw: impl FnMut(&mut Ui)) -> FullOutput {
    harness(Settings { theme, ..Settings::default() }, CARD, draw).output().clone()
}

/// The gallery shapes that wear a status colour without a status icon or word beside them, each named exactly (story,
/// status and size) with the reason it is accepted. Most are controls whose own label says what the colour says
/// ("Delete"); an entry that stops matching fails, so it goes the moment the shape does.
struct Known {
    story: &'static str,
    status: Status,
    /// Width and height in points, rounded.
    size: (i32, i32),
    /// Text that must be painted in the same story, so the exception cannot outlive the cue it relies on.
    needs: &'static [&'static str],
    /// Where each needed cue must be painted, relative to the shape.
    zone: Zone,
    why: &'static str,
}

/// Where a cue must sit relative to the shape that relies on it, so the same words elsewhere in the story (another
/// component's triangle) cannot stand in.
#[derive(Clone, Copy)]
enum Zone {
    /// On the same row: the cue's vertical span overlaps the shape's (a card header at the far end of the row).
    Row,
    /// Within this many points of the shape (a label inside it or a message under it).
    Near(f32),
}

const DESTRUCTIVE: &str =
    "a destructive control: its label names the action (\"Delete\"), the danger colour only repeats it";

const KNOWN: &[Known] = &[
    Known {
        story: "step_card",
        status: Status::Warn,
        size: (76, 57),
        needs: &[icons::status(Status::Warn), "Gate failed"],
        zone: Zone::Row,
        why: "the warn border round a failed gate's evidence picture; the card's header carries the triangle and the words",
    },
    Known {
        story: "button",
        status: Status::Danger,
        size: (108, 28),
        needs: &["Delete 3 parts"],
        zone: Zone::Near(0.0),
        why: DESTRUCTIVE,
    },
    Known {
        story: "button",
        status: Status::Danger,
        size: (64, 28),
        needs: &["Delete"],
        zone: Zone::Near(0.0),
        why: DESTRUCTIVE,
    },
    Known {
        story: "dialog",
        status: Status::Danger,
        size: (162, 28),
        needs: &["Delete"],
        zone: Zone::Near(0.0),
        why: DESTRUCTIVE,
    },
    Known {
        story: "dialog",
        status: Status::Danger,
        size: (107, 28),
        needs: &["Delete"],
        zone: Zone::Near(0.0),
        why: DESTRUCTIVE,
    },
    Known {
        story: "text_field",
        status: Status::Danger,
        size: (220, 28),
        needs: &["Use letters and numbers"],
        zone: Zone::Near(40.0),
        why: "the error border; the message is the cue, with no status icon yet (follow-up filed)",
    },
    Known {
        story: "palette",
        status: Status::Warn,
        size: (64, 52),
        needs: &["warn"],
        zone: Zone::Near(40.0),
        why: "a palette swatch, named by its token; it shows the colour and is not a state",
    },
    Known {
        story: "palette",
        status: Status::Danger,
        size: (64, 52),
        needs: &["danger"],
        zone: Zone::Near(40.0),
        why: "a palette swatch, named by its token; it shows the colour and is not a state",
    },
];

/// The entry of [`KNOWN`] that accepts `bare` in `story`, if any: same story, status and size, and every cue it relies
/// on painted where its [`Zone`] says, among `cues` (the story's painted text and where it sits).
fn known_index(story: &str, bare: Bare, cues: &[(String, Rect)]) -> Option<usize> {
    KNOWN.iter().position(|k| {
        let placed = |at: &Rect| match k.zone {
            Zone::Row => at.min.y < bare.rect.max.y && at.max.y > bare.rect.min.y,
            Zone::Near(reach) => at.intersects(bare.rect.expand(reach)),
        };
        k.story == story
            && k.status == bare.status
            && k.size == size_of(bare.rect)
            && k.needs.iter().all(|cue| cues.iter().any(|(text, at)| text.contains(cue) && placed(at)))
    })
}

/// The shapes of one frame that no exception accepts. Each exception accepts one shape per frame: a second one of the
/// same size is a new bare mark. `used` records which exceptions were needed.
fn unaccepted(story: &str, bares: &[Bare], cues: &[(String, Rect)], used: &mut [bool]) -> Vec<Bare> {
    let mut taken = vec![false; KNOWN.len()];
    let mut left = Vec::new();
    for bare in bares {
        match known_index(story, *bare, cues) {
            Some(index) if !taken[index] => {
                taken[index] = true;
                used[index] = true;
            }
            _ => left.push(*bare),
        }
    }
    left
}

#[allow(clippy::cast_possible_truncation, reason = "a widget is a few hundred points across")]
fn size_of(rect: Rect) -> (i32, i32) {
    (rect.width().round() as i32, rect.height().round() as i32)
}

#[test]
fn every_gallery_story_pairs_each_status_colour_with_its_icon_or_word() {
    let mut problems = Vec::new();
    let mut used = vec![false; KNOWN.len()];
    for kind in ThemeKind::ALL {
        for story in all() {
            let mut state = (story.new_state)();
            let output = frame_of(kind, |ui| (story.show)(ui, &mut *state));
            let cues: Vec<(String, Rect)> = painted(&output).into_iter().map(|p| (p.text, p.rect)).collect();
            let bares = colour_alone(&output, Theme::of(kind));
            for bare in unaccepted(story.id, &bares, &cues, &mut used) {
                problems.push(format!("{} ({kind:?}): a {:?} shape at {:?}", story.id, bare.status, bare.rect));
            }
        }
    }
    assert!(problems.is_empty(), "{} states signalled by colour alone:\n{}", problems.len(), problems.join("\n"));
    for (index, k) in KNOWN.iter().enumerate() {
        assert!(used[index], "{} no longer has that bare {:?} shape ({}); remove its entry", k.story, k.status, k.why);
    }
}

/// A status-coloured mark of one kind, drawn next to `label`, as the planted cases below need.
fn dot_beside(label: &str, paint: impl Fn(&egui::Painter, Rect)) -> FullOutput {
    frame_of(ThemeKind::Dark, |ui| {
        ui.horizontal(|ui| {
            let (rect, _) = ui.allocate_exact_size(vec2(12.0, 12.0), egui::Sense::hover());
            paint(ui.painter(), rect);
            if !label.is_empty() {
                ui.label(label);
            }
        });
    })
}

/// How a planted mark is painted into its 12 pt slot.
type Paint = Box<dyn Fn(&egui::Painter, Rect)>;

fn circle(painter: &egui::Painter, rect: Rect) {
    painter.circle_filled(rect.center(), 6.0, DARK.danger);
}

#[test]
fn a_planted_status_dot_with_no_cue_or_an_unrelated_label_is_found() {
    assert_eq!(colour_alone(&dot_beside("", circle), &DARK).len(), 1, "no label at all");
    assert_eq!(colour_alone(&dot_beside("Render job 12", circle), &DARK).len(), 1, "an unrelated label is no cue");
    assert_eq!(colour_alone(&dot_beside("Warning", circle), &DARK).len(), 1, "another status's word is no cue");
}

#[test]
fn a_planted_status_dot_with_its_icon_or_word_passes() {
    assert_eq!(colour_alone(&dot_beside("Error", circle), &DARK).len(), 0);
    assert_eq!(colour_alone(&dot_beside(icons::status(Status::Danger), circle), &DARK).len(), 0);
}

#[test]
fn a_bare_path_line_mesh_or_ellipse_in_a_status_colour_is_found_too() {
    let danger = DARK.danger;
    let primitives: [(&str, Paint); 5] = [
        (
            "path",
            Box::new(move |p, r| {
                p.add(Shape::convex_polygon(
                    vec![r.left_top(), r.right_top(), r.center_bottom()],
                    danger,
                    Stroke::NONE,
                ));
            }),
        ),
        (
            "line",
            Box::new(move |p, r| {
                p.line_segment([r.left_top(), r.right_bottom()], Stroke::new(2.0, danger));
            }),
        ),
        (
            "mesh",
            Box::new(move |p, r| {
                let mut mesh = egui::Mesh::default();
                mesh.add_colored_rect(r, danger);
                p.add(Shape::mesh(mesh));
            }),
        ),
        (
            "ellipse",
            Box::new(move |p, r| {
                p.add(Shape::ellipse_filled(r.center(), vec2(8.0, 5.0), danger));
            }),
        ),
        (
            "stroked rect",
            Box::new(move |p, r| {
                p.rect_stroke(r, 2.0, Stroke::new(1.5, danger), egui::StrokeKind::Inside);
            }),
        ),
    ];
    for (name, paint) in primitives {
        assert_eq!(colour_alone(&dot_beside("", &paint), &DARK).len(), 1, "a bare {name}");
        assert_eq!(colour_alone(&dot_beside("Error", &paint), &DARK).len(), 0, "a {name} with its word");
    }
}

/// Text inside or beside a status-coloured shape is not a cue unless it is that status's own icon or word.
#[test]
fn a_status_bordered_card_with_unrelated_text_inside_is_found() {
    let card = |label: &'static str| {
        frame_of(ThemeKind::Dark, move |ui| {
            let (rect, _) = ui.allocate_exact_size(vec2(200.0, 40.0), egui::Sense::hover());
            ui.painter().rect_stroke(rect, 4.0, Stroke::new(2.0, DARK.danger), egui::StrokeKind::Inside);
            ui.put(rect.shrink(8.0), egui::Label::new(label));
        })
    };
    assert_eq!(colour_alone(&card("Render job 12"), &DARK).len(), 1);
    assert_eq!(colour_alone(&card("Error: render job 12 failed"), &DARK).len(), 0);
}

/// The word is matched whole: "book" and "okay" contain "ok" but say nothing about a status.
#[test]
fn a_word_that_merely_contains_ok_is_not_the_ok_cue() {
    let ok_dot = |p: &egui::Painter, r: Rect| {
        p.circle_filled(r.center(), 6.0, DARK.ok);
    };
    for label in ["book", "okay", "Cookbook"] {
        assert_eq!(colour_alone(&dot_beside(label, ok_dot), &DARK).len(), 1, "{label}");
    }
    assert_eq!(colour_alone(&dot_beside("OK", ok_dot), &DARK).len(), 0);
    assert_eq!(colour_alone(&dot_beside("Passed: OK", ok_dot), &DARK).len(), 0);
}

/// The `step_card` story's real frame, its bare shapes and its painted text.
fn step_card_frame() -> (Vec<Bare>, Vec<(String, Rect)>) {
    let Some(story) = all().into_iter().find(|s| s.id == "step_card") else {
        panic!("the step_card story is registered")
    };
    let mut state = (story.new_state)();
    let output = frame_of(ThemeKind::Dark, |ui| (story.show)(ui, &mut *state));
    let cues = painted(&output).into_iter().map(|p| (p.text, p.rect)).collect();
    (colour_alone(&output, &DARK), cues)
}

/// An exception holds only for its exact shape, once, and only while the cues it relies on are painted in its row.
/// Checked against the real gallery frame, where the metric chip paints another warning triangle elsewhere.
#[test]
fn the_step_card_exception_needs_its_own_header_cues_and_covers_one_shape() {
    let warning = icons::status(Status::Warn);
    let (bares, cues) = step_card_frame();
    assert!(
        cues.iter().filter(|(t, _)| t.contains(warning)).count() >= 2,
        "a chip triangle exists besides the header's"
    );
    let mut used = vec![false; KNOWN.len()];
    assert!(unaccepted("step_card", &bares, &cues, &mut used).is_empty(), "the real frame is accepted");
    assert!(used.iter().any(|u| *u));

    // The header's triangle is gone; the chip's triangle (another row) must not stand in for it.
    let Some(gate) = bares.iter().find(|b| size_of(b.rect) == (76, 57)) else { panic!("the evidence border is bare") };
    let without_header_triangle: Vec<_> = cues
        .iter()
        .filter(|(t, at)| !(t.contains(warning) && at.min.y < gate.rect.max.y && at.max.y > gate.rect.min.y))
        .cloned()
        .collect();
    assert_eq!(unaccepted("step_card", &bares, &without_header_triangle, &mut vec![false; KNOWN.len()]).len(), 1);

    // The words are gone.
    let without_words: Vec<_> = cues.iter().filter(|(t, _)| !t.contains("Gate failed")).cloned().collect();
    assert_eq!(unaccepted("step_card", &bares, &without_words, &mut vec![false; KNOWN.len()]).len(), 1);

    // A second bare mark of the same size is not covered.
    let mut doubled = bares.clone();
    doubled.push(*gate);
    assert_eq!(unaccepted("step_card", &doubled, &cues, &mut vec![false; KNOWN.len()]).len(), 1);

    // Nor is a different size, or another story.
    let other = Bare { rect: Rect::from_min_size(gate.rect.min, vec2(80.0, 20.0)), ..*gate };
    assert_eq!(unaccepted("step_card", &[other], &cues, &mut vec![false; KNOWN.len()]).len(), 1);
    assert_eq!(unaccepted("tab", &[*gate], &cues, &mut vec![false; KNOWN.len()]).len(), 1);
}
