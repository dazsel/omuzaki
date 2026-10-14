//! The words components add on their own, in the places the gallery's still frame does not show: tooltips, the error
//! bubble, the context menu, the colour popover, an armed or rejecting approval. Each scenario runs in the
//! pseudo-locale with accented caller strings, so any plain English that is left was written into the component.

use std::sync::Arc;

use egui_kittest::Harness;

use super::*;
use crate::ds::audit::plain_english;
use crate::ds::strings::{English, Pseudo};
use crate::ds::testing::all_strings;
use crate::ds::theme::set_strings;

/// A harness in the pseudo-locale.
fn speaking_pseudo<S: 'static>(
    state: S,
    size: egui::Vec2,
    app: impl FnMut(&mut egui::Ui, &mut S) + 'static,
) -> Harness<'static, S> {
    let mut h = harness_state(Settings::default(), size, state, app);
    set_strings(&h.ctx, Arc::new(Pseudo(English)));
    h.run_steps(3);
    h
}

fn assert_no_english<S>(h: &Harness<'_, S>, what: &str) {
    let strings = all_strings(h);
    assert!(strings.len() > 3, "{what}: the scan saw only {strings:?}");
    let english = plain_english(strings.iter().map(String::as_str));
    assert!(english.is_empty(), "{what}: plain English from a component: {english:?}");
}

/// The scenario really reached the transient UI: some string on screen or in the tree contains `part`.
fn assert_shows<S>(h: &Harness<'_, S>, part: &str, what: &str) {
    assert!(all_strings(h).iter().any(|s| s.contains(part)), "{what}: nothing contains {part}");
}

/// The first node of `role` whose accessible name contains `part` (the pseudo-locale wraps a message in brackets and
/// pads it, so the exact name is not worth spelling out).
fn find<'h, S>(h: &'h Harness<'_, S>, role: Role, part: &str) -> egui_kittest::Node<'h> {
    let found = h.get_all_by_role(role).find(|n| n.accesskit_node().label().is_some_and(|l| l.contains(part)));
    assert!(found.is_some(), "no {role:?} named like {part}");
    found.unwrap_or_else(|| h.root())
}

/// Wait for a tooltip: egui shows it after a delay measured in its own frame time.
fn hover(h: &mut Harness<'_, impl Sized>, at: egui::Pos2) {
    h.hover_at(at);
    h.run_steps(10);
}

#[test]
fn a_param_slider_says_nothing_in_english_when_hovered_edited_locked_or_right_clicked() {
    struct Rig {
        value: f64,
        locked: f64,
    }
    let mut h = speaking_pseudo(Rig { value: 0.62, locked: 0.3 }, vec2(420.0, 160.0), |ui, r| {
        ParamSlider::new("Ŵíďţĥ", &mut r.value, 0.0..=1.0).default_value(0.5).unit("°").show(ui);
        ParamSlider::new("Ĺöçķéď", &mut r.locked, 0.0..=1.0).default_value(0.5).locked_by("Ďéšíĝñéŕ").show(ui);
    });
    assert_no_english(&h, "at rest");
    // The reset button's tooltip, and the locked slider's lock.
    let reset = find(&h, Role::Button, "Ŕéšéť").rect().center();
    hover(&mut h, reset);
    assert_no_english(&h, "reset tooltip");
    assert_shows(&h, "Ŕéšéť ťö", "reset tooltip");
    hover(&mut h, pos2(5.0, 5.0));
    // A typed entry that is not a number shows the error bubble.
    find(&h, Role::Button, "Éďíť").click();
    h.run_steps(3);
    find(&h, Role::TextInput, "Ŵíďţĥ").type_text("ñöţ á ñüṁḃéŕ");
    h.run();
    press(&mut h, Modifiers::NONE, Key::Enter);
    h.run_steps(3);
    assert_no_english(&h, "error bubble");
    assert_shows(&h, "Éñťéŕ", "error bubble");
    press(&mut h, Modifiers::NONE, Key::Escape);
    h.run_steps(3);
    // The context menu.
    h.get_by_role_and_label(Role::Slider, "Ŵíďţĥ").click_secondary();
    h.run_steps(4);
    assert_no_english(&h, "context menu");
    assert_shows(&h, "Çöðý", "context menu");
}

#[test]
fn the_colour_popover_and_the_field_say_nothing_in_english() {
    let mut h = speaking_pseudo(Colour::rgb(0x8A, 0x6F, 0xD1), vec2(360.0, 520.0), |ui, colour| {
        let palette = [PaletteSlot { name: "hair.root", colour: Colour::rgb(0x8A, 0x6F, 0xD1) }];
        ColourField::new("Ĥáíŕ ŕöö ţ", colour)
            .alpha(true)
            .linked("hair.root")
            .palette(&palette)
            .recent(&[Colour::rgb(1, 2, 3)])
            .neighbour(PaletteSlot { name: "skin.base", colour: Colour::rgb(0xF2, 0xC9, 0xB0) })
            .show(ui);
    });
    assert_no_english(&h, "closed field");
    h.get_by_role_and_label(Role::ColorWell, "Ĥáíŕ ŕöö ţ").click();
    h.run_steps(4);
    assert_no_english(&h, "open popover");
    assert_shows(&h, "Ħüé", "open popover");
}

#[test]
fn an_approval_card_says_nothing_in_english_armed_or_rejecting() {
    let mut h = speaking_pseudo(Approval { decision: None }, vec2(520.0, 520.0), |ui, a| {
        a.decision = a.decision.take().or_else(|| {
            ApprovalCard::new("live", "Ďéšíĝñéŕ", "Ĝö ļíṿé")
                .scope("broadcast")
                .reversible(false)
                .similar(3)
                .variant(ApprovalVariant::Dangerous { confirm: "Çöñƒíŕṁ" })
                .show(ui)
        });
    });
    assert_no_english(&h, "dangerous card");
    // Arm it: the confirm button, and Cancel, appear.
    let arm = h
        .get_all_by_role(Role::Button)
        .find(|b| b.accesskit_node().label().is_some_and(|l| l.contains("Å")))
        .map(|b| b.rect().center());
    if let Some(at) = arm {
        h.hover_at(at);
        h.run();
        h.event(egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: Modifiers::NONE,
        });
        h.event(egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: Modifiers::NONE,
        });
        h.run();
    }
    assert_no_english(&h, "armed card");
    assert_shows(&h, "Çáñçéļ", "armed card");
    // Reject opens the reason box.
    let reject = h
        .get_all_by_role(Role::Button)
        .find(|b| b.accesskit_node().label().is_some_and(|l| l.contains("Ŕ")))
        .map(|b| b.rect().center());
    if let Some(at) = reject {
        h.hover_at(at);
        h.run();
        h.event(egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: Modifiers::NONE,
        });
        h.event(egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: Modifiers::NONE,
        });
        h.run_steps(4);
    }
    assert_no_english(&h, "rejecting card");
    assert_shows(&h, "Þáçķ", "rejecting card");
}

#[test]
fn rows_meters_and_fields_say_nothing_in_english_when_hovered_or_renamed() {
    let mut h = speaking_pseudo(String::from("Ĥáíŕ"), vec2(420.0, 360.0), |ui, name| {
        let icons = [
            RowIcon::Visible(true),
            RowIcon::Locked { owner: "Ďéšíĝñéŕ" },
            RowIcon::AgentTouched { by: "Ďéšíĝñéŕ" },
            RowIcon::GateWarning { text: "ļíķéñéšš" },
        ];
        TreeRow::new(1, "Ĥáíŕ", crate::ds::icons::CUBE).icons(&icons).show(ui);
        TreeRow::new(2, "Ŕéñáṁíñĝ", crate::ds::icons::CUBE).renaming(name).show(ui);
        let channels = [Channel { raw: 0.4, clipping: true, confidence: 0.2, ..Channel::new("Ĵáŵ", 0.5) }];
        MeterGrid::new("Ĉĥáññéļš", &channels).show(ui);
        let mut text = String::from("áḃç");
        ui.add(TextField::new("Ñáṁé", &mut text).error("Ŵŕöñĝ"));
        ui.add(ActorChip::new("Ďéšíĝñéŕ", ActorKind::Agent));
        ui.add(StatusChip::new(Status::Warn, "Ďéĝŕáďéď"));
        ui.add(GateChip::failed("ļíķéñéšš", "0.8", "0.9"));
        ui.add(CostChip::new(&["~4 š"]));
        Banner::new(Status::Danger, "Ŕéñďéŕ ƒáíļéď").show(ui);
    });
    assert_no_english(&h, "rows, chips and fields");
    let row = h.get_by_role_and_label(Role::TreeItem, "Ĥáíŕ").rect();
    hover(&mut h, pos2(row.right() - 14.0, row.center().y));
    assert_no_english(&h, "state icon tooltip");
    assert_shows(&h, "Ğáťé", "state icon tooltip");
    let grid = h.get_by_role_and_label(Role::Group, "Ĉĥáññéļš").rect();
    hover(&mut h, grid.center());
    assert_no_english(&h, "meter tooltip");
    assert_shows(&h, "öŵñéŕ", "meter tooltip");
}

#[test]
fn toasts_say_nothing_in_english() {
    let mut h = speaking_pseudo((), vec2(480.0, 200.0), |ui, ()| {
        let toast = Toast::new(Status::Ok, "Ďéšíĝñéŕ ĉĥáñĝéď").action("Ûñďö");
        ToastCard::new(&toast).count(3).show(ui);
    });
    h.run_steps(2);
    assert_no_english(&h, "toast card");
}

#[test]
fn dropdowns_tab_bars_and_dialogs_say_nothing_in_english_open_hovered_armed_or_closed() {
    use crate::ds::strings::pseudo;
    struct Rig {
        part: usize,
        tab: usize,
        dialog: bool,
    }
    // A message inside another ("Close {label}" wraps the label's own brackets) is found by its letters alone.
    let core = |text: &str| pseudo(text).trim_matches(['[', ']', '·']).to_owned();
    let mut h = speaking_pseudo(Rig { part: 0, tab: 0, dialog: false }, vec2(640.0, 560.0), |ui, r| {
        let (part, dark, light) = (pseudo("Part"), pseudo("Dark"), pseudo("Light"));
        let (why, workspaces, build, theatre) =
            (pseudo("Not built yet"), pseudo("Workspaces"), pseudo("Build"), pseudo("Theatre"));
        ui.add(Dropdown::new(&part, &mut r.part, &[Choice::new(0, &dark), Choice::new(1, &light).disabled(&why)]));
        let tabs = [
            TabItem::new(&build).closable(),
            TabItem::new(&theatre).icon(crate::ds::icons::ROBOT).icon_only().agent_active(true),
        ];
        ui.add(TabBar::new(&workspaces, &mut r.tab, &tabs));
        if ui.add(Button::new(&pseudo("Open"))).clicked() {
            r.dialog = true;
        }
        if r.dialog {
            let (title, primary, confirm, body) =
                (pseudo("Delete 3 parts?"), pseudo("Delete 3 parts"), pseudo("Confirm: delete"), pseudo("They go."));
            let action = Dialog::new("d", &title, &primary).dangerous(&confirm).show(ui.ctx(), |ui| {
                ui.label(&body);
            });
            r.dialog &= action.is_none();
        }
    });
    assert_no_english(&h, "closed dropdown and tab bar");
    // The words a tab says on its own: its close button and the agent dot.
    find(&h, Role::Button, &core("Close"));
    assert_shows(&h, &core("an agent is working here"), "the agent dot, in words");
    // The open list, with the tooltip of the row that cannot be chosen.
    find(&h, Role::ComboBox, &core("Part")).focus();
    h.run();
    press(&mut h, Modifiers::NONE, Key::Enter);
    h.run_steps(3);
    assert_no_english(&h, "open dropdown");
    let disabled = find(&h, Role::ListBoxOption, &core("Light")).rect().center();
    hover(&mut h, disabled);
    assert_no_english(&h, "the tooltip of a row that cannot be chosen");
    assert_shows(&h, &core("Not built yet"), "the reason");
    press(&mut h, Modifiers::NONE, Key::Escape);
    // The dialog, closed on its safe choice, then armed.
    find(&h, Role::Button, &core("Open")).click();
    h.run_steps(4);
    assert_no_english(&h, "dialog");
    assert_shows(&h, &core("Cancel"), "the cancel button");
    let arm = find(&h, Role::Button, &core("Delete 3 parts")).rect().center();
    h.hover_at(arm);
    h.run();
    for pressed in [true, false] {
        h.event(egui::Event::PointerButton {
            pos: arm,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        });
    }
    h.run_steps(3);
    assert_no_english(&h, "armed dialog");
    assert_shows(&h, &core("Confirm: delete"), "the confirm label");
}

#[test]
fn rows_cards_tiles_curves_and_the_slider_menu_say_nothing_in_english() {
    use crate::ds::strings::pseudo;
    use crate::ds::widgets::OpCall;
    struct Rig {
        value: f64,
        curve: Curve,
    }
    let core = |text: &str| pseudo(text).trim_matches(['[', ']', '·']).to_owned();
    let mut h =
        speaking_pseudo(Rig { value: 0.4, curve: Curve::unit(Preset::SCurve) }, vec2(520.0, 1500.0), |ui, r| {
            let (job, error, step, who, cause, tile, locked, curve, slider) = (
                pseudo("Render sheets"),
                pseudo("Out of memory"),
                pseudo("Rounder face"),
                pseudo("Designer"),
                pseudo("make it oval"),
                pseudo("Oval"),
                pseudo("Locked by Designer"),
                pseudo("Jaw response"),
                pseudo("Width"),
            );
            JobRow::new(&job, JobState::Failed { error: &error })
                .cost(&["~40 s"])
                .elapsed("0:17")
                .id_salt("failed")
                .show(ui);
            JobRow::new(&job, JobState::Running { progress: None }).id_salt("running").show(ui);
            let metrics = [Metric { name: &pseudo("likeness"), delta: "+0.03", change: Change::Worse }];
            StepCard::new(&step, &who, ActorKind::Agent)
                .cause(&cause)
                .metrics(&metrics)
                .age("4 s")
                .state(StepState::FailedGate)
                .pictures(Thumb::Missing, Thumb::Loading)
                .evidence(Thumb::Missing)
                .id_salt("failed")
                .show(ui);
            ThumbnailTile::new(&tile, Thumb::Missing).disabled(&locked).id_salt("tile").show(ui);
            CurveEditor::new(&curve, &mut r.curve).plot_height(80.0).id_salt("curve").show(ui);
            let slider = ParamSlider::new(&slider, &mut r.value, 0.0..=1.0)
                .default_value(0.5)
                .op_call(OpCall { op: "part.set_param", target: "\"part\":\"hair\"" });
            slider.show(ui);
        });
    assert_no_english(&h, "a failed job, a failed gate, a tile, a curve and a slider at rest");
    // The words a job, a card and a tile say on their own.
    for part in
        ["Retry", "Failed", "Gate failed", "No preview", "Before", "Evidence", "Rewind", "Preset", "Input", "Tangent"]
    {
        assert_shows(&h, &core(part), part);
    }
    // The tooltip of a tile that cannot be chosen.
    let tile = find(&h, Role::Button, &core("Oval")).rect().center();
    hover(&mut h, tile);
    assert_no_english(&h, "the tooltip of a disabled tile");
    hover(&mut h, pos2(5.0, 5.0));
    // The message under a number that is not one.
    find(&h, Role::SpinButton, &core("Output")).click();
    h.run_steps(3);
    let typing = find(&h, Role::TextInput, &core("value"));
    typing.focus();
    h.run();
    find(&h, Role::TextInput, &core("value")).type_text("ñöť á ñüṁḃéŕ");
    h.run();
    press(&mut h, Modifiers::NONE, Key::Enter);
    h.run_steps(3);
    assert_no_english(&h, "the message under a number that is not one");
    assert_shows(&h, &core("Enter a number from"), "the message");
    press(&mut h, Modifiers::NONE, Key::Escape);
    h.run_steps(3);
    // The slider's menu, with the op call and History entries.
    find(&h, Role::Slider, &core("Width")).click_secondary();
    h.run_steps(4);
    assert_no_english(&h, "the slider menu");
    for part in ["Paste value", "Copy op call", "Show in History"] {
        assert_shows(&h, &core(part), part);
    }
}
