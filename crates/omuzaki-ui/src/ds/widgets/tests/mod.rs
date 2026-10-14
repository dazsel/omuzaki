//! Behaviour and accessibility tests for the components, through `egui_kittest`. Each one operates the widget the
//! way a person would, with the pointer or with the keyboard alone, and checks what AccessKit exposes.

use egui::accesskit::{Role, Toggled};
use egui::{Id, Key, Modifiers, pos2, vec2};
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use omuzaki_core::colour::Colour;

use super::*;
use crate::ds::testing::{harness_state, panel_harness, press};
use crate::ds::theme::Settings;
use crate::ds::tokens::Status;

const SIZE: egui::Vec2 = vec2(360.0, 300.0);

/// Whether `node` sits inside a node of `role`. egui puts generic containers between a list and its items, so the
/// parent is not always the list itself.
fn inside(node: egui_kittest::kittest::AccessKitNode<'_>, role: Role) -> bool {
    let mut parent = node.parent();
    while let Some(p) = parent {
        if p.role() == role {
            return true;
        }
        parent = p.parent();
    }
    false
}

fn state<S: 'static>(state: S, app: impl FnMut(&mut egui::Ui, &mut S) + 'static) -> egui_kittest::Harness<'static, S> {
    harness_state(Settings::default(), SIZE, state, app)
}

// ---- Button ------------------------------------------------------------------------------------------------------

#[test]
fn a_button_is_clicked_by_pointer_enter_and_space() {
    let mut h = state(0u32, |ui, n| *n += u32::from(ui.add(Button::new("Approve").primary()).clicked()));
    h.get_by_role_and_label(Role::Button, "Approve").click();
    h.run();
    assert_eq!(*h.state(), 1);
    h.get_by_role_and_label(Role::Button, "Approve").focus();
    h.run();
    press(&mut h, Modifiers::NONE, Key::Enter);
    press(&mut h, Modifiers::NONE, Key::Space);
    assert_eq!(*h.state(), 3, "Enter and Space both press a focused button");
}

#[test]
fn a_disabled_button_keeps_its_place_says_why_and_does_nothing() {
    let mut h =
        state(0u32, |ui, n| *n += u32::from(ui.add(Button::new("Approve").disabled("Select a part first")).clicked()));
    let node = h.get_by_role_and_label(Role::Button, "Approve");
    assert!(node.accesskit_node().is_disabled());
    assert_eq!(node.accesskit_node().description().as_deref(), Some("Select a part first"));
    node.click();
    h.run();
    assert_eq!(*h.state(), 0);
}

#[test]
fn the_accessible_label_can_say_more_than_the_visible_one() {
    let h = state((), |ui, ()| {
        ui.add(Button::new("Approve").accessible_label("Approve: render 24 sheets (Designer)"));
    });
    h.get_by_role_and_label(Role::Button, "Approve: render 24 sheets (Designer)");
}

// ---- Toggle ------------------------------------------------------------------------------------------------------

#[test]
fn a_switch_toggles_by_label_click_and_space_and_reports_its_state() {
    let mut h = state(false, |ui, on| {
        ui.add(Toggle::switch("Live preview", on));
    });
    let node = h.get_by_role_and_label(Role::Switch, "Live preview");
    assert_eq!(node.accesskit_node().toggled(), Some(Toggled::False));
    node.click();
    h.run();
    assert!(*h.state());
    assert_eq!(h.get_by_role_and_label(Role::Switch, "Live preview").accesskit_node().toggled(), Some(Toggled::True));
    h.get_by_role_and_label(Role::Switch, "Live preview").focus();
    h.run();
    press(&mut h, Modifiers::NONE, Key::Space);
    assert!(!*h.state(), "Space toggles a focused switch");
}

#[test]
fn a_parent_checkbox_is_tri_state_and_a_click_on_mixed_turns_everything_on() {
    let mut h = state(TriState::Mixed, |ui, s| {
        ui.add(Toggle::tri("Outfit", s));
    });
    let node = h.get_by_role_and_label(Role::CheckBox, "Outfit");
    assert_eq!(node.accesskit_node().toggled(), Some(Toggled::Mixed));
    node.click();
    h.run();
    assert_eq!(*h.state(), TriState::On);
}

// ---- Segmented ---------------------------------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Side {
    Left,
    Centre,
    Right,
}

fn segmented() -> egui_kittest::Harness<'static, Side> {
    state(Side::Centre, |ui, side| {
        ui.add(Segmented::new(
            "Fringe side",
            side,
            &[
                Segment::new(Side::Left, "Left"),
                Segment::new(Side::Centre, "Centre"),
                Segment::new(Side::Right, "Right"),
            ],
        ));
        ui.add(Button::new("After"));
    })
}

#[test]
fn a_segmented_control_is_a_radio_group_with_one_selected_radio() {
    let h = segmented();
    h.get_by_role_and_label(Role::RadioGroup, "Fringe side");
    let selected: Vec<_> = ["Left", "Centre", "Right"]
        .into_iter()
        .map(|l| h.get_by_role_and_label(Role::RadioButton, l).accesskit_node().toggled() == Some(Toggled::True))
        .collect();
    assert_eq!(selected, [false, true, false]);
}

#[test]
fn arrow_keys_move_and_select_and_the_group_is_one_tab_stop() {
    let mut h = segmented();
    h.get_by_role_and_label(Role::RadioButton, "Centre").focus();
    h.run();
    press(&mut h, Modifiers::NONE, Key::ArrowRight);
    assert_eq!(*h.state(), Side::Right);
    press(&mut h, Modifiers::NONE, Key::ArrowLeft);
    press(&mut h, Modifiers::NONE, Key::ArrowLeft);
    assert_eq!(*h.state(), Side::Left);
    // Tab leaves the group: the next stop is the button after it, not the next segment.
    press(&mut h, Modifiers::NONE, Key::Tab);
    h.run();
    assert!(h.get_by_role_and_label(Role::Button, "After").is_focused(), "Tab moves past the whole group");
}

#[test]
fn segmented_controls_sit_side_by_side_in_a_left_to_right_row() {
    // The group used to pull the cursor back over itself, so the next control of a row was drawn on top of it.
    let h = harness_state(Settings::default(), vec2(900.0, 100.0), (), |ui, ()| {
        ui.horizontal(|ui| {
            let (mut a, mut b) = (0u8, 0u8);
            ui.add(Segmented::new("First", &mut a, &[Segment::new(0u8, "One"), Segment::new(1u8, "Two")]));
            ui.add(Segmented::new("Second", &mut b, &[Segment::new(0u8, "Three"), Segment::new(1u8, "Four")]));
            ui.add(Button::new("After"));
        });
    });
    let (first, second, after) = (
        h.get_by_role_and_label(Role::RadioGroup, "First").rect(),
        h.get_by_role_and_label(Role::RadioGroup, "Second").rect(),
        h.get_by_role_and_label(Role::Button, "After").rect(),
    );
    assert!(second.left() >= first.right(), "{first:?} then {second:?}");
    assert!(after.left() >= second.right(), "{second:?} then {after:?}");
}

#[test]
fn clicking_a_segment_selects_it() {
    let mut h = segmented();
    h.get_by_role_and_label(Role::RadioButton, "Right").click();
    h.run();
    assert_eq!(*h.state(), Side::Right);
}

// ---- TreeRow -----------------------------------------------------------------------------------------------------

struct Tree {
    selected: bool,
    open: bool,
    hidden_clicked: bool,
}

#[test]
fn a_tree_row_is_a_tree_item_with_level_selection_and_expansion() {
    let mut h = state(Tree { selected: false, open: false, hidden_clicked: false }, |ui, t| {
        let icons = [RowIcon::Visible(true)];
        let out = TreeRow::new("hair", "Hair", crate::ds::icons::CUBE)
            .level(2)
            .expand(if t.open { Expand::Open } else { Expand::Closed })
            .selected(t.selected, true)
            .icons(&icons)
            .show(ui);
        match out.hit {
            Some(RowHit::Chevron) => t.open = !t.open,
            Some(RowHit::Row) => t.selected = true,
            Some(RowHit::Icon(_)) => t.hidden_clicked = true,
            None => {}
        }
    });
    let node = h.get_by_role_and_label(Role::TreeItem, "Hair");
    let ak = node.accesskit_node();
    assert_eq!((ak.level(), ak.data().is_expanded(), ak.is_selected()), (Some(3), Some(false), Some(false)));
    let rect = node.rect();
    // The chevron sits in the second 16 pt indent column of a level-2 row; the eye is at the right edge.
    h.get_by_role_and_label(Role::TreeItem, "Hair").hover();
    for (pos, _) in [
        (pos2(rect.left() + 32.0 + 8.0, rect.center().y), "chevron"),
        (pos2(rect.right() - 14.0, rect.center().y), "eye"),
        (pos2(rect.left() + 90.0, rect.center().y), "row"),
    ] {
        h.hover_at(pos);
        h.run();
        h.event(egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: Modifiers::NONE,
        });
        h.event(egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: Modifiers::NONE,
        });
        h.run();
    }
    assert!(h.state().open, "the chevron expanded it");
    assert!(h.state().hidden_clicked, "the state icon took its own click");
    assert!(h.state().selected, "the row body selected it");
}

// ---- TextField ---------------------------------------------------------------------------------------------------

#[test]
fn escape_clears_a_text_field_then_leaves_it() {
    let mut h = state("twintails".to_owned(), |ui, text| {
        ui.add(TextField::new("Search parts", text).search().hint("Search"));
    });
    let field = h.get_by_role_and_label(Role::TextInput, "Search parts");
    field.focus();
    h.run();
    press(&mut h, Modifiers::NONE, Key::Escape);
    assert_eq!(h.state().as_str(), "", "the first Esc clears");
    assert!(h.get_by_role_and_label(Role::TextInput, "Search parts").is_focused(), "and keeps focus");
    press(&mut h, Modifiers::NONE, Key::Escape);
    assert!(!h.get_by_role_and_label(Role::TextInput, "Search parts").is_focused(), "the second Esc leaves");
}

#[test]
fn the_clear_button_is_labelled_and_empties_the_field() {
    let mut h = state("abc".to_owned(), |ui, text| {
        ui.add(TextField::new("Part name", text));
    });
    h.get_by_role_and_label(Role::Button, "Clear Part name").click();
    h.run();
    assert_eq!(h.state().as_str(), "");
}

#[test]
fn a_text_field_in_a_vertical_layout_leaves_the_next_widget_below_it() {
    // The field's inner `Ui` used to set the cursor to a point above the field's bottom edge, so the next widget of a
    // vertical layout (a dialog's buttons, a form row's help line) overlapped the field.
    let h = state("abc".to_owned(), |ui, text| {
        ui.vertical(|ui| {
            // No item spacing: the only thing that can separate them is where the field's inner `Ui` left the cursor.
            ui.spacing_mut().item_spacing.y = 0.0;
            ui.add(TextField::new("Part name", text));
            ui.add(Button::new("After"));
        });
    });
    let field = h.get_by_role_and_label(Role::TextInput, "Part name").rect();
    let after = h.get_by_role_and_label(Role::Button, "After").rect();
    // The text node is the field less its 1 pt inset, so the button belongs at `bottom + 1`; with the old
    // `scope_builder` it sat at `bottom`, inside the field.
    assert!(after.top() >= field.bottom() + 0.99, "{field:?} then {after:?}");
}

// ---- Section -----------------------------------------------------------------------------------------------------

#[test]
fn sections_toggle_and_ctrl_click_opens_one_and_collapses_the_rest() {
    let group = Id::new("sections");
    let mut h = state((), move |ui, ()| {
        Section::new("Essentials").default_open(true).group(group).show(ui, |ui| {
            ui.add(Button::new("In essentials"));
        });
        Section::new("Advanced").default_open(true).group(group).show(ui, |ui| {
            ui.add(Button::new("In advanced"));
        });
    });
    h.get_by_label("In essentials");
    h.get_by_label("In advanced");
    h.get_by_role_and_label(Role::DisclosureTriangle, "Essentials").click();
    h.run_steps(8);
    assert!(h.query_by_label("In essentials").is_none(), "a click closes an open section");
    h.get_by_role_and_label(Role::DisclosureTriangle, "Essentials").click_modifiers(Modifiers::COMMAND);
    h.run_steps(8);
    assert!(h.query_by_label("In essentials").is_some(), "Ctrl+click opens this one");
    assert!(h.query_by_label("In advanced").is_none(), "and collapses its siblings");
}

// ---- Toasts ------------------------------------------------------------------------------------------------------

/// A context whose clock is fake: the test moves time by hand, whatever the frame rate.
fn toast_context() -> (egui::Context, std::sync::Arc<omuzaki_core::time::FakeClock>) {
    let ctx = egui::Context::default();
    let clock = std::sync::Arc::new(omuzaki_core::time::FakeClock::new(omuzaki_core::time::Seconds(0.0)));
    crate::ds::clock::set_clock(&ctx, std::sync::Arc::clone(&clock) as std::sync::Arc<dyn omuzaki_core::time::Clock>);
    (ctx, clock)
}

/// One frame of the toast stack. The input says a frame took 1/60 s, which is what egui reports after a sleep.
fn frame(ctx: &egui::Context, toasts: &mut Toasts, pointer: Option<egui::Pos2>) -> Vec<ToastEvent> {
    let mut input = egui::RawInput {
        predicted_dt: 1.0 / 60.0,
        screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, vec2(800.0, 600.0))),
        ..egui::RawInput::default()
    };
    if let Some(pos) = pointer {
        input.events.push(egui::Event::PointerMoved(pos));
    }
    let mut events = Vec::new();
    ctx.run_ui(input, |ui| {
        crate::ds::apply_settings(ui.ctx(), Settings::default());
        events = toasts.show(ui.ctx(), vec2(16.0, 40.0));
    })
    .drop_without_applying_deltas();
    events
}

fn advance(clock: &omuzaki_core::time::FakeClock, seconds: f64) {
    clock.advance(omuzaki_core::time::Seconds(seconds));
}

#[test]
fn toasts_expire_by_status_fold_duplicates_log_everything_and_errors_stay() {
    let (ctx, clock) = toast_context();
    let mut toasts = Toasts::default();
    for _ in 0..3 {
        advance(&clock, 0.1);
        frame(&ctx, &mut toasts, None); // warm up fonts
    }
    toasts.push(&ctx, Toast::new(Status::Ok, "Designer changed the face profile"));
    toasts.push(&ctx, Toast::new(Status::Ok, "Designer changed the face profile"));
    toasts.push(&ctx, Toast::new(Status::Danger, "Render failed"));
    assert_eq!(toasts.visible(), 2, "the duplicate folded into one toast");
    assert_eq!(toasts.log().len(), 3, "but all three are in the log");
    for _ in 0..30 {
        advance(&clock, 0.25);
        frame(&ctx, &mut toasts, None);
    }
    assert_eq!(toasts.visible(), 1, "the success toast left after 4 s; the error stays until dismissed");
    assert_eq!(toasts.log().len(), 3);
}

#[test]
fn at_most_three_toasts_show_and_the_oldest_leave_first() {
    let (ctx, _clock) = toast_context();
    let mut toasts = Toasts::default();
    for i in 0..5 {
        toasts.push(&ctx, Toast::new(Status::Info, format!("Message {i}")));
    }
    assert_eq!(toasts.visible(), 3);
    assert_eq!(toasts.log().len(), 5);
}

// ---- ApprovalCard ------------------------------------------------------------------------------------------------

pub(super) struct Approval {
    pub(super) decision: Option<Decision>,
}

pub(super) fn approval(
    variant: ApprovalVariant<'static>,
    scope: &'static str,
) -> egui_kittest::Harness<'static, Approval> {
    harness_state(Settings::default(), vec2(520.0, 460.0), Approval { decision: None }, move |ui, a| {
        let card = ApprovalCard::new("req", "Designer", "Render 24 contact sheets")
            .scope(scope)
            .cost(&["~40 s", "1.2 GB"])
            .reversible(true)
            .variant(variant);
        a.decision = a.decision.take().or_else(|| card.show(ui));
    })
}

#[test]
fn approving_returns_the_decision_and_each_button_names_the_agent_and_action() {
    let mut h = approval(ApprovalVariant::Request, "render");
    h.get_by_role_and_label(Role::Button, "Approve for this session: Render 24 contact sheets (Designer)");
    h.get_by_role_and_label(Role::Button, "Reject: Render 24 contact sheets (Designer)");
    h.get_by_role_and_label(Role::Button, "Approve: Render 24 contact sheets (Designer)").click();
    h.run();
    assert_eq!(h.state().decision, Some(Decision::Approve));
}

#[test]
fn rejecting_with_a_comment_sends_it_back_as_the_reason() {
    let mut h = approval(ApprovalVariant::Request, "render");
    h.get_by_role_and_label(Role::Button, "Reject: Render 24 contact sheets (Designer)").click();
    h.run_steps(3);
    h.get_by_role_and_label(Role::TextInput, "Reason for rejecting").type_text("Use the soft light rig");
    h.run();
    h.get_by_role_and_label(Role::Button, "Reject and tell Designer").click();
    h.run();
    assert_eq!(h.state().decision, Some(Decision::Reject { comment: Some("Use the soft light rig".to_owned()) }));
}

#[test]
fn ctrl_enter_approves_for_the_session_when_focus_is_in_the_card() {
    let mut h = approval(ApprovalVariant::Request, "render");
    h.get_by_role_and_label(Role::Button, "Approve: Render 24 contact sheets (Designer)").focus();
    h.run();
    press(&mut h, Modifiers::CTRL, Key::Enter);
    assert_eq!(h.state().decision, Some(Decision::ApproveForSession));
}

// ---- MeterGrid ---------------------------------------------------------------------------------------------------

#[test]
fn a_meter_grid_announces_the_focused_channel_with_raw_and_owner() {
    let mut h = state((), |ui, ()| {
        let channels = [
            Channel { raw: 0.45, ..Channel::new("Jaw open", 0.52) },
            Channel { owner: Owner::Audio, ..Channel::new("Mouth funnel", 0.3) },
        ];
        MeterGrid::new("Face channels", &channels).columns(1).show(ui);
    });
    let grid = h.get_by_role_and_label(Role::Group, "Face channels");
    grid.focus();
    h.run();
    assert_eq!(
        h.get_by_role_and_label(Role::Group, "Face channels").value().as_deref(),
        Some("Jaw open: 0.52, raw 0.45, owner tracker")
    );
    press(&mut h, Modifiers::NONE, Key::ArrowDown);
    assert_eq!(
        h.get_by_role_and_label(Role::Group, "Face channels").value().as_deref(),
        Some("Mouth funnel: 0.30, owner audio")
    );
}

#[test]
fn an_idle_meter_grid_asks_for_no_more_frames() {
    let mut h = state((), |ui, ()| {
        let channels = [Channel::new("Jaw open", 0.52), Channel::new("Eye blink", 0.1)];
        MeterGrid::new("Face channels", &channels).show(ui);
    });
    h.run_steps(20);
    assert!(!h.ctx.has_requested_repaint(), "values that are not moving cost nothing (P12)");
}

// ---- ColourField -------------------------------------------------------------------------------------------------

struct ColourRig {
    colour: Colour,
    events: Vec<ColourEvent>,
}

fn colour_harness() -> egui_kittest::Harness<'static, ColourRig> {
    harness_state(
        Settings::default(),
        vec2(360.0, 460.0),
        ColourRig { colour: Colour::rgb(0x8A, 0x6F, 0xD1), events: Vec::new() },
        |ui, c| {
            let palette = [PaletteSlot { name: "hair.root", colour: Colour::rgb(0x8A, 0x6F, 0xD1) }];
            let out = ColourField::new("Hair root colour", &mut c.colour)
                .palette(&palette)
                .neighbour(PaletteSlot { name: "skin.base", colour: Colour::rgb(0xF2, 0xC9, 0xB0) })
                .show(ui);
            c.events.extend(out.event);
        },
    )
}

#[test]
fn a_colour_field_names_its_colour_and_opens_a_popover_with_hex_hue_and_contrast() {
    let mut h = colour_harness();
    let node = h.get_by_role_and_label(Role::ColorWell, "Hair root colour");
    assert_eq!(node.value().as_deref(), Some("#8A6FD1"));
    node.click();
    h.run_steps(3);
    h.get_by_role_and_label(Role::TextInput, "Hex colour");
    h.get_by_role_and_label(Role::Slider, "Hue");
    assert!(h.query_by_label_contains("value contrast").is_some(), "the contrast chip against the neighbouring slot");
}

#[test]
fn hex_entry_previews_live_and_enter_commits_one_transaction() {
    let mut h = colour_harness();
    h.get_by_role_and_label(Role::ColorWell, "Hair root colour").click();
    h.run_steps(3);
    let hex = h.get_by_role_and_label(Role::TextInput, "Hex colour");
    hex.focus();
    h.run();
    h.get_by_role_and_label(Role::TextInput, "Hex colour").type_text("112233");
    h.run();
    assert_eq!(h.state().colour, Colour::rgb(0x11, 0x22, 0x33), "typing previews live");
    press(&mut h, Modifiers::NONE, Key::Enter);
    h.run_steps(2);
    let commits: Vec<_> = h.state().events.iter().filter(|e| matches!(e, ColourEvent::Commit { .. })).collect();
    assert_eq!(commits.len(), 1, "{:?}", h.state().events);
    assert!(h.query_by_role_and_label(Role::Slider, "Hue").is_none(), "Enter closes the popover");
}

#[test]
fn escape_cancels_and_restores_the_colour() {
    let mut h = colour_harness();
    let original = h.state().colour;
    h.get_by_role_and_label(Role::ColorWell, "Hair root colour").click();
    h.run_steps(3);
    h.get_by_role_and_label(Role::Slider, "Hue").focus();
    h.run();
    press(&mut h, Modifiers::SHIFT, Key::ArrowRight);
    assert_ne!(h.state().colour, original, "the hue bar moved the colour");
    press(&mut h, Modifiers::NONE, Key::Escape);
    h.run_steps(2);
    assert_eq!(h.state().colour, original);
    assert!(h.state().events.iter().any(|e| matches!(e, ColourEvent::Cancel(_))));
}

// ---- Chips and banners are named for screen readers ------------------------------------------------------------

#[test]
fn chips_banners_and_empty_states_are_exposed_with_names() {
    let h = panel_harness(|ui| {
        ui.add(ActorChip::new("Designer", ActorKind::Agent));
        ui.add(ScopeBadge::new("broadcast"));
        ui.add(StatusChip::new(Status::Warn, "Degraded"));
        ui.add(GateChip::failed("likeness", "0.81", "0.85"));
        Banner::new(Status::Warn, "Tracking lost").action("Reconnect").show(ui);
    });
    h.get_by_label("Designer");
    h.get_by_label("broadcast");
    h.get_by_label("Degraded");
    let gate = h.get_by_role_and_label(Role::Button, "likeness");
    assert_eq!(gate.value().as_deref(), Some("0.81, below 0.85, gate failed"));
    h.get_by_role_and_label(Role::Button, "Reconnect");
}

#[test]
fn a_label_that_adds_to_its_name_does_not_replace_it() {
    // egui reads a label's text from its `value`, so a node that sets a value of its own hides the text. A folded toast
    // says its message and the count; a cost chip says what it is and its parts.
    let h = panel_harness(|ui| {
        let toast = Toast::new(Status::Ok, "Autosaved 14:32");
        ToastCard::new(&toast).count(3).show(ui);
        ui.add(CostChip::new(&["~40 s", "1.2 GB"]));
    });
    assert_eq!(
        h.get_by_role(Role::Status).value().as_deref(),
        Some("Autosaved 14:32, repeated 3 times"),
        "the message is still there, with the count"
    );
    h.get_by_label("Estimated cost: ~40 s, 1.2 GB");
}

mod approval;
mod colour;
mod curve_editor;
mod dialog;
mod dropdown;
mod form_row;
mod id_salt;
mod job_row;
mod keyboard;
mod layout_cursor;
mod long_labels;
mod number_field;
mod param_extras;
mod pseudo;
mod step_card;
mod tab;
mod thumbnail;
mod toast;
