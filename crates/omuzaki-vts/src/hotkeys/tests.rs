// Test setup may panic: a panic is a clear failure.
#![allow(clippy::unwrap_used)]

use super::*;

const FRAME_60: Duration = Duration::from_nanos(16_666_667);

fn hotkey(name: &str, id: &str) -> Hotkey {
    Hotkey {
        name: name.into(),
        kind: "ToggleExpression".into(),
        description: String::new(),
        file: String::new(),
        hotkey_id: id.into(),
        on_screen_button_id: -1,
    }
}

#[test]
fn a_second_trigger_of_one_hotkey_waits_five_frames_and_is_not_dropped() {
    let mut limiter = HotkeyLimiter::new(60);
    let mut out = Vec::new();
    limiter.request("smug".into());
    limiter.due(Duration::ZERO, &mut out);
    assert_eq!(out, ["smug"]);
    out.clear();
    limiter.request("smug".into());
    for frame in 1..5 {
        limiter.due(FRAME_60 * frame, &mut out);
        assert!(out.is_empty(), "frame {frame}: still cooling down");
    }
    assert_eq!(limiter.next_due(), Some(FRAME_60 * 5));
    limiter.due(FRAME_60 * 5, &mut out);
    assert_eq!(out, ["smug"], "sent once the five frames have passed");
    assert_eq!(limiter.dropped(), 0);
}

#[test]
fn forty_triggers_in_one_frame_send_thirty_two_and_count_eight() {
    let mut limiter = HotkeyLimiter::new(60);
    for i in 0..40 {
        limiter.request(format!("hotkey{i}"));
    }
    let mut out = Vec::new();
    limiter.due(Duration::ZERO, &mut out);
    assert_eq!(out.len(), 32);
    assert_eq!(limiter.dropped(), 8);
    // The queue drains one hotkey a frame, so one more fits a frame later.
    limiter.request("late".into());
    out.clear();
    limiter.due(FRAME_60, &mut out);
    assert_eq!(out, ["late"]);
}

#[test]
fn the_limiter_never_sends_what_the_mock_would_refuse() {
    // The mock's rules (cooldown and queue) are the documented ones; the limiter mirrors them with a frame rounded up.
    let mut limiter = HotkeyLimiter::new(0);
    assert_eq!(limiter.frame, Duration::from_nanos(33_333_334), "an unknown rate counts as 30 fps");
    limiter.set_framerate(144);
    assert_eq!(limiter.frame, Duration::from_nanos(6_944_445));
}

#[test]
fn suggestions_match_hotkey_names_by_word_and_leave_the_rest() {
    let hotkeys = [hotkey("Smug", "h1"), hotkey("Angry Face", "h2"), hotkey("Wave", "h3")];
    let rows = suggest(&["smug", "angry", "happy", "sad"], &hotkeys);
    let mapped: Vec<_> = rows.iter().map(|r| (r.omuzaki.as_str(), &r.target)).collect();
    assert_eq!(
        mapped,
        [
            ("smug", &Target::Hotkey { id: "h1".into(), name: "Smug".into() }),
            ("angry", &Target::Hotkey { id: "h2".into(), name: "Angry Face".into() }),
        ]
    );
    assert!(!rows.iter().any(|r| matches!(&r.target, Target::Hotkey { name, .. } if name == "Wave")));
    assert_eq!(suggest(&["agnry"], &hotkeys).len(), 0, "two edits is not a match");
    assert_eq!(suggest(&["angri"], &hotkeys)[0].omuzaki, "angri", "one edit is");
}

#[test]
fn a_mapping_resolves_per_model_and_round_trips_as_json() {
    let mut mapping = VtsMapping::default();
    mapping.models.insert(
        "model-a".into(),
        vec![
            Row { omuzaki: "smug".into(), target: Target::Hotkey { id: "h1".into(), name: "Smug".into() } },
            Row { omuzaki: "cry".into(), target: Target::Expression { file: "Cry.exp3.json".into(), fade_s: 0.5 } },
        ],
    );
    assert!(matches!(mapping.resolve("model-a", "smug"), Some(Target::Hotkey { id, .. }) if id == "h1"));
    assert_eq!(mapping.resolve("model-b", "smug"), None);
    let json = serde_json::to_string(&mapping).unwrap();
    assert!(json.contains(r#""kind":"expression""#));
    assert_eq!(serde_json::from_str::<VtsMapping>(&json).unwrap(), mapping);
}
