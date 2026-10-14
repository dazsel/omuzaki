//! Step cards: the actions each state offers, named for the step; the states in words; lazy pictures.

use egui::TextureId;
use egui_kittest::Harness;

use super::*;
use crate::ds::testing::unlabelled_interactive;

struct Rig {
    state: StepState,
    action: Option<StepAction>,
    wants: Wants,
}

fn rig(state: StepState) -> Harness<'static, Rig> {
    harness_state(
        Settings::default(),
        vec2(380.0, 420.0),
        Rig { state, action: None, wants: Wants::default() },
        |ui, r| {
            let metrics = [
                Metric { name: "likeness", delta: "+0.03", change: Change::Better },
                Metric { name: "spacing", delta: "+0.02", change: Change::Worse },
            ];
            let out = StepCard::new("Rounder face profile", "Designer", ActorKind::Agent)
                .cause("\"make the face more oval\"")
                .metrics(&metrics)
                .age("4 s")
                .state(r.state)
                .pictures(Thumb::Ready(TextureId::Managed(1)), Thumb::Loading)
                .evidence(Thumb::Loading)
                .show(ui);
            r.action = out.action.or(r.action);
            r.wants = out.wants;
        },
    )
}

fn names(h: &Harness<'_, Rig>) -> Vec<String> {
    h.query_all_by_role(Role::Button).filter_map(|b| b.accesskit_node().label()).collect()
}

#[test]
fn each_state_offers_its_own_actions_named_for_the_step() {
    let cases = [
        (StepState::Applied, vec!["Rewind", "Explain", "Adjust"]),
        (StepState::FailedGate, vec!["Rewind", "Explain", "Adjust"]),
        (StepState::Pending, vec!["Approve", "Reject", "Explain"]),
        (StepState::Rejected, vec!["Restore", "Explain"]),
    ];
    for (state, actions) in cases {
        let h = rig(state);
        let expected: Vec<String> = actions.iter().map(|a| format!("{a}: Rounder face profile")).collect();
        assert_eq!(names(&h), expected, "{state:?}");
        assert_eq!(unlabelled_interactive(&h), Vec::<String>::new(), "{state:?}");
    }
}

#[test]
fn a_press_returns_the_action_and_the_buttons_are_stops_in_reading_order() {
    let mut h = rig(StepState::Pending);
    h.get_by_role_and_label(Role::Button, "Approve: Rounder face profile").click();
    h.run();
    assert_eq!(h.state().action, Some(StepAction::Approve));
    let mut h = rig(StepState::Applied);
    press(&mut h, Modifiers::NONE, Key::Tab);
    assert!(h.get_by_role_and_label(Role::Button, "Rewind: Rounder face profile").is_focused());
    press(&mut h, Modifiers::NONE, Key::Tab);
    press(&mut h, Modifiers::NONE, Key::Tab);
    assert!(h.get_by_role_and_label(Role::Button, "Adjust: Rounder face profile").is_focused());
    press(&mut h, Modifiers::NONE, Key::Enter);
    assert_eq!(h.state().action, Some(StepAction::Adjust), "the keyboard alone operates the card");
}

#[test]
fn the_card_is_a_group_named_for_the_actor_and_the_step_and_its_state_is_its_value_and_painted_in_words() {
    for (state, word) in [
        (StepState::Applied, "Applied"),
        (StepState::Pending, "Pending review"),
        (StepState::Rejected, "Rejected"),
        (StepState::FailedGate, "Gate failed"),
    ] {
        let h = rig(state);
        let group = h.get_by_role_and_label(Role::Group, "Designer: Rounder face profile");
        assert_eq!(group.value().as_deref(), Some(word));
        let painted = crate::ds::audit::painted(h.output());
        assert_eq!(
            painted.iter().any(|p| p.text == word),
            state != StepState::Applied,
            "{state:?}: the word is on the card"
        );
    }
}

#[test]
fn only_the_pictures_that_are_on_screen_and_missing_are_wanted_and_evidence_belongs_to_a_failed_gate() {
    let h = rig(StepState::Applied);
    assert_eq!(h.state().wants, Wants { before: false, after: true, evidence: false });
    let h = rig(StepState::FailedGate);
    assert_eq!(h.state().wants, Wants { before: false, after: true, evidence: true });
    assert!(h.state().wants.any());
}

#[test]
fn a_rejected_step_is_struck_through_and_the_card_never_grows_past_its_width() {
    let h = harness_state(Settings::default(), vec2(260.0, 420.0), (), |ui, ()| {
        let metrics = [
            Metric { name: "likeness", delta: "+0.03", change: Change::Better },
            Metric { name: "spacing error", delta: "-0.012", change: Change::Better },
            Metric { name: "mouth", delta: "+0.00", change: Change::Neutral },
        ];
        StepCard::new("A step whose label is much longer than the card is wide", "Designer", ActorKind::Agent)
            .cause("\"a prompt that goes on and on and on and on and on\"")
            .metrics(&metrics)
            .state(StepState::Rejected)
            .show(ui);
    });
    let group = h.get_by_role(Role::Group).rect();
    for button in h.query_all_by_role(Role::Button) {
        assert!(button.rect().right() <= group.right() + 0.5, "{:?} sticks out of {group:?}", button.rect());
    }
    assert!(
        crate::ds::audit::painted(h.output()).iter().any(|p| p.elided),
        "the long label and prompt end in an ellipsis"
    );
}

#[test]
fn two_cards_at_the_same_height_each_keep_their_own_cause_tooltip() {
    use crate::ds::audit::painted;
    let long_a = "A prompt for the first card that is far too long to fit on one line of it";
    let long_b = "A prompt for the second card that is also far too long to fit on its line";
    let mut h = harness_state(Settings::default(), vec2(720.0, 300.0), (), move |ui, ()| {
        let top = ui.cursor().min;
        for (i, cause) in [long_a, long_b].into_iter().enumerate() {
            let rect = egui::Rect::from_min_size(top + vec2(350.0 * i as f32, 0.0), vec2(330.0, 280.0));
            ui.scope_builder(egui::UiBuilder::new().max_rect(rect), |ui| {
                StepCard::new("Rounder face profile", "Designer", ActorKind::Agent).cause(cause).id_salt(i).show(ui);
            });
        }
    });
    // Card B's cause line: find it by its painted (elided) text.
    let b = painted(h.output()).into_iter().find(|p| p.text == long_b && p.elided).map(|p| p.rect.center());
    h.hover_at(b.unwrap_or_default());
    h.run_steps(14);
    let shown = painted(h.output());
    assert!(shown.iter().any(|p| p.text == long_b && !p.elided), "B's whole cause is shown");
    assert!(!shown.iter().any(|p| p.text == long_a && !p.elided), "and A's is not");
}
