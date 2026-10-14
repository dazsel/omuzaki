//! The two-step confirm of a dangerous request: a held Enter and a double-click must not arm and confirm in one go.

use egui::{Event, Pos2};
use egui_kittest::Harness;

use super::*;
use crate::ds::confirm::ARM_DELAY;

const GO_LIVE: &str = "Go live on Twitch as @omuzakidev";
const ARM: &str = "Approve…: Go live on Twitch as @omuzakidev (Designer)";
const CONFIRM: &str = "Confirm: go live as @omuzakidev: Go live on Twitch as @omuzakidev (Designer)";

/// A dangerous card on a fake clock the test moves.
fn dangerous() -> (Harness<'static, Approval>, std::sync::Arc<omuzaki_core::time::FakeClock>) {
    let mut h = harness_state(Settings::default(), vec2(520.0, 460.0), Approval { decision: None }, |ui, a| {
        let card = ApprovalCard::new("live", "Designer", GO_LIVE)
            .scope("broadcast")
            .variant(ApprovalVariant::Dangerous { confirm: "Confirm: go live as @omuzakidev" });
        a.decision = a.decision.take().or_else(|| card.show(ui));
    });
    let clock = std::sync::Arc::new(omuzaki_core::time::FakeClock::new(omuzaki_core::time::Seconds(0.0)));
    crate::ds::clock::set_clock(&h.ctx, std::sync::Arc::clone(&clock) as std::sync::Arc<dyn omuzaki_core::time::Clock>);
    h.run();
    (h, clock)
}

fn wait(clock: &omuzaki_core::time::FakeClock, seconds: f64) {
    clock.advance(omuzaki_core::time::Seconds(seconds));
}

/// A point inside the first button of the card, which is at the same place armed or not.
fn button_point(h: &Harness<'_, Approval>, label: &str) -> Pos2 {
    let rect = h.get_by_role_and_label(Role::Button, label).rect();
    pos2(rect.left() + 8.0, rect.center().y)
}

/// A press and a release at `at`, in the frames a real pointer takes.
fn click_at(h: &mut Harness<'_, Approval>, at: Pos2) {
    h.hover_at(at);
    h.run();
    for pressed in [true, false] {
        h.event(Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        });
        h.run();
    }
}

fn key(h: &Harness<'_, Approval>, pressed: bool, repeat: bool) {
    h.event(Event::Key {
        key: Key::Enter,
        physical_key: Some(Key::Enter),
        pressed,
        repeat,
        modifiers: Modifiers::NONE,
    });
}

#[test]
fn a_dangerous_request_needs_two_deliberate_presses_and_never_times_out() {
    let (mut h, clock) = dangerous();
    let at = button_point(&h, ARM);
    click_at(&mut h, at);
    assert_eq!(h.state().decision, None, "the first press only arms the button");
    h.get_by_role_and_label(Role::Button, CONFIRM);
    // No timer ever decides: a very long wait changes nothing.
    wait(&clock, 3600.0);
    h.run_steps(60);
    assert_eq!(h.state().decision, None, "an approval never auto-accepts or auto-rejects");
    click_at(&mut h, at);
    assert_eq!(h.state().decision, Some(Decision::Approve), "a fresh press long after the dwell confirms");
}

#[test]
fn the_second_click_of_a_double_click_does_not_confirm() {
    let (mut h, clock) = dangerous();
    let at = button_point(&h, ARM);
    click_at(&mut h, at);
    // Frames later, with only a human's double-click interval on the clock.
    wait(&clock, 0.15);
    click_at(&mut h, at);
    assert_eq!(h.state().decision, None, "a double-click arms, it does not also confirm");
    // The dwell has to pass before a press counts, and the press has to start after it.
    wait(&clock, ARM_DELAY);
    click_at(&mut h, at);
    assert_eq!(h.state().decision, Some(Decision::Approve));
}

#[test]
fn a_press_that_began_before_the_dwell_ended_does_not_confirm_when_released_after_it() {
    let (mut h, clock) = dangerous();
    let at = button_point(&h, ARM);
    click_at(&mut h, at);
    h.event(Event::PointerButton {
        pos: at,
        button: egui::PointerButton::Primary,
        pressed: true,
        modifiers: Modifiers::NONE,
    });
    h.run();
    wait(&clock, ARM_DELAY + 0.1);
    h.event(Event::PointerButton {
        pos: at,
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: Modifiers::NONE,
    });
    h.run();
    assert_eq!(h.state().decision, None, "the press began too early, however long it was held");
}

#[test]
fn a_held_enter_does_not_arm_and_confirm() {
    let (mut h, clock) = dangerous();
    h.get_by_role_and_label(Role::Button, ARM).focus();
    h.run();
    // The key goes down once and then auto-repeats, as an operating system delivers a held key.
    key(&h, true, false);
    h.run();
    h.get_by_role_and_label(Role::Button, CONFIRM);
    for _ in 0..5 {
        wait(&clock, ARM_DELAY);
        key(&h, true, true);
        h.run();
    }
    assert_eq!(h.state().decision, None, "repeats of the key that armed it never confirm, however long it is held");
    key(&h, false, false);
    h.run();
    assert_eq!(h.state().decision, None);
}

#[test]
fn a_fresh_enter_after_the_dwell_confirms_and_so_does_space() {
    for confirm_key in [Key::Enter, Key::Space] {
        let (mut h, clock) = dangerous();
        h.get_by_role_and_label(Role::Button, ARM).focus();
        h.run();
        press(&mut h, Modifiers::NONE, Key::Enter);
        assert_eq!(h.state().decision, None);
        press(&mut h, Modifiers::NONE, confirm_key);
        assert_eq!(h.state().decision, None, "a quick second key press is too soon");
        wait(&clock, ARM_DELAY);
        press(&mut h, Modifiers::NONE, confirm_key);
        assert_eq!(h.state().decision, Some(Decision::Approve), "{confirm_key:?} after the dwell confirms");
    }
}

#[test]
fn an_assistive_technology_click_is_a_press_too_and_obeys_the_dwell() {
    let (mut h, clock) = dangerous();
    let activate = |h: &mut Harness<'_, Approval>, label: &str| {
        let (node, tree) = h.get_by_role_and_label(Role::Button, label).accesskit_node().locate();
        h.event(Event::AccessKitActionRequest(egui::accesskit::ActionRequest {
            action: egui::accesskit::Action::Click,
            target_tree: tree,
            target_node: node,
            data: None,
        }));
        h.run();
    };
    activate(&mut h, ARM);
    activate(&mut h, CONFIRM);
    assert_eq!(h.state().decision, None, "an immediate second activation is ignored");
    wait(&clock, ARM_DELAY);
    activate(&mut h, CONFIRM);
    assert_eq!(h.state().decision, Some(Decision::Approve));
}

#[test]
fn an_ordinary_request_is_still_one_press() {
    let mut h = approval(ApprovalVariant::Request, "render");
    h.get_by_role_and_label(Role::Button, "Approve: Render 24 contact sheets (Designer)").click();
    h.run();
    assert_eq!(h.state().decision, Some(Decision::Approve));
}
