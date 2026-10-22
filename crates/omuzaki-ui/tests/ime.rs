//! IME composition in every text-accepting component (SE-02): preedit shows at the caret and is not yet in the value,
//! commit inserts, Enter and Esc during composition belong to the IME, and the IME's cursor rectangle follows the
//! caret. Number entry takes full-width digits and rejects other text with the field's own error.
//!
//! The harness feeds `Event::Ime` the way the platform integration does: one frame per event, so "the frame the
//! commit arrived in" and "the frame after" are told apart. The platform's own composition window is not here
//! (platform verification is QG-08).

use egui::accesskit::Role;
use egui::{Event, ImeEvent, Key, Modifiers, TextEdit, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable as _;
use omuzaki_core::colour::Colour;
use omuzaki_ui::ds::testing::{harness_state, press};
use omuzaki_ui::ds::widgets::{Button, ColourEvent, ColourField, NumberField, ParamSlider, TextField, TreeRow};
use omuzaki_ui::ds::{Settings, icons};

/// Runs one frame carrying `event`.
fn ime<S>(h: &mut Harness<'_, S>, event: ImeEvent) {
    h.event(Event::Ime(event));
    h.run_steps(1);
}

fn preedit<S>(h: &mut Harness<'_, S>, text: &str) {
    ime(h, ImeEvent::Preedit { text: text.to_owned(), active_range_chars: None });
}

fn commit<S>(h: &mut Harness<'_, S>, text: &str) {
    ime(h, ImeEvent::Commit(text.to_owned()));
}

/// Runs one frame carrying `event` and then an Enter press: the harness spreads queued events over several frames, but
/// the Enter that confirms a candidate can arrive in the frame of the commit, so both go straight into the input.
fn commit_with_enter<S>(h: &mut Harness<'_, S>, text: &str) {
    let events = &mut h.input_mut().events;
    events.push(Event::Ime(ImeEvent::Commit(text.to_owned())));
    for pressed in [true, false] {
        events.push(Event::Key {
            key: Key::Enter,
            physical_key: Some(Key::Enter),
            pressed,
            repeat: false,
            modifiers: Modifiers::NONE,
        });
    }
    h.run_steps(1);
}

/// The text of the focused edit as a screen reader reads it: the preedit is part of what is on screen.
fn shown<S>(h: &Harness<'_, S>, label: &str) -> String {
    h.get_by_role_and_label(Role::TextInput, label).value().unwrap_or_default()
}

/// The IME's cursor rectangle as the last frame reported it to the platform.
fn cursor_x<S>(h: &Harness<'_, S>) -> Option<f32> {
    h.output().platform_output.ime.map(|ime| ime.cursor_rect.center().x)
}

// ---- TextField ----------------------------------------------------------------------------------------------------

#[derive(Default)]
struct Field {
    text: String,
    submitted: u32,
}

fn text_field(initial: &str) -> Harness<'static, Field> {
    let state = Field { text: initial.to_owned(), submitted: 0 };
    let mut h = harness_state(Settings::default(), vec2(360.0, 120.0), state, |ui, s| {
        let response = ui.add(TextField::new("Search", &mut s.text).search());
        s.submitted += u32::from(response.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)));
    });
    h.get_by_role_and_label(Role::TextInput, "Search").focus();
    h.run_steps(2);
    // Focus by keyboard selects the text; the caret goes to the end so a preedit lands after "ab".
    press(&mut h, Modifiers::NONE, Key::End);
    h
}

#[test]
fn a_text_field_shows_the_preedit_at_the_caret_without_putting_it_in_the_value() {
    let mut h = text_field("ab");
    let before = cursor_x(&h);
    preedit(&mut h, "かん");
    assert_eq!(shown(&h, "Search"), "abかん", "the preedit is on screen at the caret");
    assert_eq!(h.state().text, "ab", "and not yet in the value");
    preedit(&mut h, "かんじ");
    assert_eq!(shown(&h, "Search"), "abかんじ");
    assert_eq!(h.state().text, "ab");
    assert!(cursor_x(&h) > before, "the IME cursor rectangle follows the caret: {before:?} -> {:?}", cursor_x(&h));
}

#[test]
fn a_text_field_inserts_the_commit_and_drops_the_preedit() {
    let mut h = text_field("ab");
    preedit(&mut h, "かんじ");
    commit(&mut h, "漢字");
    assert_eq!(h.state().text, "ab漢字");
    assert_eq!(shown(&h, "Search"), "ab漢字");
    preedit(&mut h, "x");
    preedit(&mut h, "");
    assert_eq!(h.state().text, "ab漢字", "a cancelled composition leaves the value alone");
    assert_eq!(shown(&h, "Search"), "ab漢字");
}

#[test]
fn enter_during_composition_goes_to_the_ime_not_to_submit() {
    let mut h = text_field("ab");
    preedit(&mut h, "か");
    press(&mut h, Modifiers::NONE, Key::Enter);
    assert_eq!(h.state().submitted, 0, "Enter while a preedit shows submits nothing");
    assert!(h.get_by_role_and_label(Role::TextInput, "Search").is_focused(), "and the field keeps focus");
    // The Enter that confirms a candidate may share a frame with the commit.
    commit_with_enter(&mut h, "化");
    assert_eq!(h.state().submitted, 0, "nor does an Enter in the frame of the commit");
    assert_eq!(h.state().text, "ab化");
    press(&mut h, Modifiers::NONE, Key::Enter);
    assert_eq!(h.state().submitted, 1, "the next Enter is an ordinary submit");
}

#[test]
fn escape_during_composition_goes_to_the_ime_not_to_clear_or_leave() {
    let mut h = text_field("ab");
    preedit(&mut h, "か");
    press(&mut h, Modifiers::NONE, Key::Escape);
    assert_eq!(h.state().text, "ab", "Esc does not clear the field");
    assert!(h.get_by_role_and_label(Role::TextInput, "Search").is_focused());
    // The IME answers its own Esc by dropping the preedit.
    preedit(&mut h, "");
    press(&mut h, Modifiers::NONE, Key::Escape);
    assert_eq!(h.state().text, "", "afterwards Esc clears as it always did");
}

#[test]
fn cancelling_a_composition_over_a_selection_gives_the_text_back() {
    let mut h = text_field("ab");
    press(&mut h, Modifiers::COMMAND, Key::A);
    preedit(&mut h, "か");
    assert_eq!(shown(&h, "Search"), "か", "the preedit is shown in place of the selection");
    assert_eq!(h.state().text, "ab", "not in the value");
    preedit(&mut h, "");
    h.run_steps(2);
    assert_eq!(h.state().text, "ab", "a cancel leaves the value alone");
    assert_eq!(shown(&h, "Search"), "ab", "and the screen shows it again");
    preedit(&mut h, "x");
    preedit(&mut h, "");
    assert_eq!(h.state().text, "ab");
}

#[test]
fn a_field_that_submits_on_enter_during_composition_is_caught() {
    // The planted field: a bare `TextEdit` that submits on Enter whatever the IME is doing.
    let mut h = harness_state(Settings::default(), vec2(360.0, 80.0), Field::default(), |ui, s| {
        let response = ui.add(TextEdit::singleline(&mut s.text).id_salt("planted"));
        s.submitted += u32::from(response.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter)));
    });
    h.get_by_role(Role::TextInput).focus();
    h.run_steps(2);
    preedit(&mut h, "か");
    press(&mut h, Modifiers::NONE, Key::Enter);
    assert_eq!(h.state().submitted, 1, "the probe sees a submit that the real fields do not make");
    assert_eq!(h.state().text, "か", "and the preedit has leaked into its value");
}

// ---- ParamSlider number entry -------------------------------------------------------------------------------------

struct Slider {
    value: f64,
}

fn slider() -> Harness<'static, Slider> {
    let mut h = harness_state(Settings::default(), vec2(360.0, 160.0), Slider { value: 0.0 }, |ui, s| {
        ParamSlider::new("Width", &mut s.value, 0.0..=100.0).show(ui);
        ui.add(Button::new("Elsewhere"));
    });
    h.get_by_role_and_label(Role::Button, "Edit Width value").click();
    h.run_steps(3);
    h
}

#[test]
fn a_slider_number_field_takes_composition_and_converts_full_width_digits() {
    let mut h = slider();
    preedit(&mut h, "１２");
    assert!(shown(&h, "Width value").contains("１２"), "the preedit shows: {}", shown(&h, "Width value"));
    assert!(cursor_x(&h).is_some(), "the IME cursor rectangle is reported while the field is focused");
    press(&mut h, Modifiers::NONE, Key::Enter);
    assert!((h.state().value - 0.0).abs() < f64::EPSILON, "Enter during composition commits nothing");
    h.get_by_role_and_label(Role::TextInput, "Width value");
    commit(&mut h, "１２");
    h.get_by_role_and_label(Role::TextInput, "Width value");
    press(&mut h, Modifiers::NONE, Key::Enter);
    assert!((h.state().value - 12.0).abs() < 1e-9, "full-width 12 reads as 12: {}", h.state().value);
}

#[test]
fn a_slider_number_field_rejects_other_text_with_its_own_error_and_escape_waits_for_the_ime() {
    let mut h = slider();
    preedit(&mut h, "あ");
    press(&mut h, Modifiers::NONE, Key::Escape);
    h.get_by_role_and_label(Role::TextInput, "Width value");
    commit_with_enter(&mut h, "あ");
    press(&mut h, Modifiers::NONE, Key::Enter);
    h.get_by_role_and_label(Role::TextInput, "Width value");
    assert!((h.state().value - 0.0).abs() < f64::EPSILON, "text that is not a number changes nothing");
    assert!(h.query_by_label_contains("Enter a number").is_some(), "and the field's own message says so");
}

// ---- NumberField ----------------------------------------------------------------------------------------------------

fn number_field() -> Harness<'static, Slider> {
    let mut h = harness_state(Settings::default(), vec2(360.0, 160.0), Slider { value: 0.0 }, |ui, s| {
        NumberField::new("Output", &mut s.value, 0.0..=100.0).show(ui);
        ui.add(Button::new("Elsewhere"));
    });
    h.get_by_role_and_label(Role::SpinButton, "Output").click();
    h.run_steps(3);
    h
}

#[test]
fn leaving_a_slider_field_mid_composition_commits_the_old_text_not_the_preedit() {
    let mut h = slider();
    preedit(&mut h, "１２");
    h.get_by_role_and_label(Role::Button, "Elsewhere").focus();
    h.run_steps(3);
    assert!((h.state().value - 0.0).abs() < f64::EPSILON, "the preedit is not a value: {}", h.state().value);
}

#[test]
fn leaving_a_number_field_mid_composition_commits_the_old_text_not_the_preedit() {
    let mut h = number_field();
    preedit(&mut h, "１２");
    h.get_by_role_and_label(Role::Button, "Elsewhere").focus();
    h.run_steps(3);
    assert!((h.state().value - 0.0).abs() < f64::EPSILON, "the preedit is not a value: {}", h.state().value);
}

#[test]
fn a_number_field_converts_full_width_digits_and_leaves_enter_and_escape_to_the_ime() {
    let mut h = number_field();
    preedit(&mut h, "４");
    press(&mut h, Modifiers::NONE, Key::Escape);
    press(&mut h, Modifiers::NONE, Key::Enter);
    h.get_by_role_and_label(Role::TextInput, "Output value");
    assert!((h.state().value - 0.0).abs() < f64::EPSILON, "neither key reached the field while composing");
    commit_with_enter(&mut h, "４２");
    h.get_by_role_and_label(Role::TextInput, "Output value");
    assert!((h.state().value - 0.0).abs() < f64::EPSILON, "the Enter of the commit frame is the IME's");
    press(&mut h, Modifiers::NONE, Key::Enter);
    assert!((h.state().value - 42.0).abs() < 1e-9, "full-width 42 reads as 42: {}", h.state().value);
}

#[test]
fn a_number_field_rejects_committed_text_that_is_not_a_number_with_its_own_error() {
    let mut h = number_field();
    commit(&mut h, "あ");
    press(&mut h, Modifiers::NONE, Key::Enter);
    h.get_by_role_and_label(Role::TextInput, "Output value");
    assert!((h.state().value - 0.0).abs() < f64::EPSILON);
    assert!(h.query_by_label_contains("Enter a number").is_some(), "the field's own message shows");
}

#[test]
fn batched_ime_events_keep_committed_text_in_order() {
    // A commit then an empty preedit (the Wayland sequence) and a commit then a new preedit, in one frame.
    let mut h = text_field("ab");
    for events in [
        vec![ImeEvent::Commit("漢字".to_owned()), ImeEvent::Preedit { text: String::new(), active_range_chars: None }],
        vec![ImeEvent::Commit("!".to_owned()), ImeEvent::Preedit { text: "か".to_owned(), active_range_chars: None }],
    ] {
        h.input_mut().events.extend(events.into_iter().map(Event::Ime));
        h.run_steps(1);
    }
    // The preedit after the commit waits a frame for the caret the commit leaves.
    h.run_steps(1);
    assert_eq!(h.state().text, "ab漢字!", "the committed text is the value; the new preedit is not");
    assert_eq!(shown(&h, "Search"), "ab漢字!か");
    commit(&mut h, "化");
    assert_eq!(h.state().text, "ab漢字!化");
}

#[test]
fn idle_empty_preedits_are_noise_and_surrounding_deletes_are_kept() {
    let mut h = text_field("ab");
    // Some platforms repeat empty preedits while nothing is composing: typing and Enter go on as usual.
    h.input_mut().events.push(Event::Ime(ImeEvent::Preedit { text: String::new(), active_range_chars: None }));
    h.input_mut().events.push(Event::Text("x".to_owned()));
    h.run_steps(1);
    assert_eq!(h.state().text, "abx", "typing after an idle empty preedit still lands");
    h.input_mut().events.push(Event::Ime(ImeEvent::Preedit { text: String::new(), active_range_chars: None }));
    h.run_steps(1);
    press(&mut h, Modifiers::NONE, Key::Enter);
    assert_eq!(h.state().submitted, 1, "and an idle empty preedit does not take the Enter");
    // Android and iOS delete around the caret on corrections and backspace. Enter left the field: go back in.
    h.get_by_role_and_label(Role::TextInput, "Search").focus();
    h.run_steps(2);
    press(&mut h, Modifiers::NONE, Key::End);
    ime(&mut h, ImeEvent::DeleteSurrounding { before_chars: 1, after_chars: 0 });
    assert_eq!(h.state().text, "ab", "the surrounding delete is part of the value");
}

/// Pushes `events` into the next frame together and runs it.
fn one_frame<S>(h: &mut Harness<'_, S>, events: Vec<Event>) {
    h.input_mut().events.extend(events);
    h.run_steps(1);
}

/// Selects characters `from..to` of the focused edit by setting its cursor state (the harness' own key events carry no
/// modifiers into the edit).
fn select<S>(h: &mut Harness<'_, S>, from: usize, to: usize) {
    use egui::text::{CCursor, CCursorRange};
    let Some(id) = h.ctx.memory(egui::Memory::focused) else { panic!("no focused edit") };
    let Some(mut state) = TextEdit::load_state(&h.ctx, id) else { panic!("no state for the focused edit") };
    state.cursor.set_char_range(Some(CCursorRange::two(CCursor::new(from), CCursor::new(to))));
    TextEdit::store_state(&h.ctx, id, state);
    h.run_steps(1);
}

/// `text` without the character `c`.
fn without(text: &str, c: char) -> String {
    text.chars().filter(|&x| x != c).collect()
}

fn empty_preedit() -> Event {
    Event::Ime(ImeEvent::Preedit { text: String::new(), active_range_chars: None })
}

#[test]
fn ordinary_edits_batched_with_composition_transitions_stay_in_the_value() {
    let mut h = text_field("ab");
    // Preedit, then in one frame: it ends, and a letter is typed.
    preedit(&mut h, "か");
    one_frame(&mut h, vec![empty_preedit(), Event::Text("x".to_owned())]);
    assert_eq!(h.state().text, "abx", "a letter typed as the composition ends is not lost");
    // A letter typed in the frame a new preedit begins.
    one_frame(
        &mut h,
        vec![
            Event::Text("y".to_owned()),
            Event::Ime(ImeEvent::Preedit { text: "か".to_owned(), active_range_chars: None }),
        ],
    );
    assert_eq!(h.state().text, "abxy", "and one typed before a new preedit is part of the value, the preedit is not");
}

#[test]
fn a_surrounding_delete_during_a_composition_over_a_selection_keeps_the_selected_text() {
    let mut h = text_field("abc");
    select(&mut h, 2, 3); // "c"
    preedit(&mut h, "か");
    assert_eq!(h.state().text, "abc", "the preedit replaces the selection on screen only");
    ime(&mut h, ImeEvent::DeleteSurrounding { before_chars: 1, after_chars: 0 });
    assert_eq!(h.state().text, "ac", "the platform's delete lands on the committed text, the selection stays");
    preedit(&mut h, "");
    h.run_steps(2);
    assert_eq!(h.state().text, "ac", "and a cancel gives the selection back where it was");
}

#[test]
fn typing_over_a_selection_before_the_preedit_in_one_frame_leaves_nothing_owed() {
    let mut h = text_field("abc");
    select(&mut h, 1, 3); // "bc"
    one_frame(
        &mut h,
        vec![
            Event::Text("x".to_owned()),
            Event::Ime(ImeEvent::Preedit { text: "か".to_owned(), active_range_chars: None }),
        ],
    );
    assert_eq!(h.state().text, "ax", "the typed letter replaced the selection; the preedit is not in the value");
    preedit(&mut h, "");
    h.run_steps(2);
    assert_eq!(h.state().text, "ax", "and a cancel does not bring the replaced text back");
}

#[test]
fn clearing_the_field_during_a_composition_ends_it() {
    let mut h = text_field("ab");
    preedit(&mut h, "か");
    h.get_by_role_and_label(Role::Button, "Clear Search").click();
    h.run_steps(4);
    assert_eq!(h.state().text, "", "the clear button empties the value");
    h.run_steps(4);
    assert_eq!(h.state().text, "", "and the composition's bookkeeping does not bring it back");
}

#[test]
fn selecting_all_then_a_preedit_in_one_frame_still_owes_the_selection() {
    let mut h = text_field("ab");
    let select_all = Event::Key {
        key: Key::A,
        physical_key: Some(Key::A),
        pressed: true,
        repeat: false,
        modifiers: Modifiers::COMMAND,
    };
    one_frame(
        &mut h,
        vec![
            Event::ModifiersChanged(Modifiers::COMMAND),
            select_all,
            Event::Ime(ImeEvent::Preedit { text: "か".to_owned(), active_range_chars: None }),
        ],
    );
    assert_eq!(h.state().text, "ab", "select-all replaces nothing; the preedit is not in the value");
    preedit(&mut h, "");
    h.run_steps(2);
    assert_eq!(h.state().text, "ab", "and a cancel leaves the text alone");
}

#[test]
fn replacing_the_text_under_a_composition_with_other_text_ends_it() {
    let mut h = text_field("ab");
    preedit(&mut h, "か");
    "wxyz".clone_into(&mut h.state_mut().text);
    h.run_steps(4);
    assert_eq!(h.state().text, "wxyz", "the owner's new text is not edited by the old composition's range");
}

#[test]
fn a_preedit_arriving_with_an_owner_reset_never_reaches_the_value() {
    let mut h = text_field("ab");
    preedit(&mut h, "か");
    "wxyz".clone_into(&mut h.state_mut().text);
    preedit(&mut h, "き");
    assert_eq!(h.state().text, "wxyz", "the new preedit is not part of the owner's new text");
    commit(&mut h, "漢");
    assert_eq!(without(&h.state().text, '漢'), "wxyz", "and a commit after it lands in the new text, at the caret");
}

#[test]
fn a_commit_arriving_with_an_owner_reset_lands_on_the_new_text_alone() {
    let mut h = text_field("ab");
    preedit(&mut h, "か");
    "wxyz".clone_into(&mut h.state_mut().text);
    commit(&mut h, "漢");
    assert_eq!(without(&h.state().text, '漢'), "wxyz");
    assert!(!h.state().text.contains('か'));
}

/// The invariant behind the whole file: text that only ever appears as a preedit (`かきく`) is never in the value, in
/// any order of preedits, commits, ordinary typing, platform deletions, caret moves, selection, select-all, cut, paste, undo,
/// redo and owner resets,
/// batched or not, over several frames.
#[test]
fn preedit_only_text_never_reaches_the_value_in_any_ordering() {
    const PREEDITS: [&str; 3] = ["か", "き", "く"];
    let mut seed =
        std::env::var("IME_PROPERTY_SEED").ok().and_then(|v| v.parse().ok()).unwrap_or(0x2545_f491_4f6c_dd1d_u64);
    let mut next = move |bound: usize| {
        // xorshift: deterministic, no dependency.
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        usize::try_from(seed % u64::try_from(bound).unwrap_or(1)).unwrap_or(0)
    };
    let key = |key: Key, modifiers: Modifiers| Event::Key {
        key,
        physical_key: Some(key),
        pressed: true,
        repeat: false,
        modifiers,
    };
    let mut clock = 10.0;
    // 400 sequences keep the suite fast; `IME_PROPERTY_ROUNDS=20000` (and `IME_PROPERTY_SEED`) runs the deep version.
    let rounds = std::env::var("IME_PROPERTY_ROUNDS").ok().and_then(|v| v.parse().ok()).unwrap_or(400);
    for round in 0..rounds {
        let mut h = text_field("ab");
        let mut log = Vec::new();
        // At least the events a preedit could have carried into the next frame (see `carried_after`).
        let mut pending: Vec<Event> = Vec::new();
        for _ in 0..8 {
            let mut events = Vec::new();
            for _ in 0..=next(3) {
                events.push(match next(14) {
                    0 | 1 => {
                        Event::Ime(ImeEvent::Preedit { text: PREEDITS[next(3)].to_owned(), active_range_chars: None })
                    }
                    2 => empty_preedit(),
                    3 => Event::Ime(ImeEvent::Commit(["漢", "字", ""][next(3)].to_owned())),
                    4 => Event::Text(["x", "y"][next(2)].to_owned()),
                    5 => Event::Ime(ImeEvent::DeleteSurrounding { before_chars: 1, after_chars: 0 }),
                    6 => key([Key::ArrowLeft, Key::Home, Key::End, Key::Backspace][next(4)], Modifiers::NONE),
                    7 => key(Key::Z, Modifiers::COMMAND),
                    8 => key(Key::Z, Modifiers { shift: true, ..Modifiers::COMMAND }),
                    9 => key(Key::A, Modifiers::COMMAND),
                    10 => key([Key::ArrowLeft, Key::ArrowRight, Key::Home, Key::End][next(4)], Modifiers::SHIFT),
                    11 => Event::Cut,
                    12 => Event::Paste(["p", "qr"][next(2)].to_owned()),
                    _ => key(Key::Enter, Modifiers::NONE),
                });
            }
            // Time passes between frames, so the editor takes its own undo points.
            clock += [0.1, 1.5, 3.0][next(3)];
            h.input_mut().time = Some(clock);
            let mut reset = next(8) == 0;
            // The editor's own selection (select-all, shift+arrows) is the person's and legitimately survives a
            // reset, as it does with no composition; only a selection a composition was standing in for must not.
            if reset {
                let id = h.ctx.memory(egui::Memory::focused);
                let state = id.and_then(|id| TextEdit::load_state(&h.ctx, id));
                let own_selection = state.and_then(|s| s.cursor.char_range()).is_some_and(|r| r.primary != r.secondary);
                reset = !own_selection;
            }
            if reset {
                "wxyz".clone_into(&mut h.state_mut().text);
                log.push("RESET".to_owned());
            }
            log.push(format!("{events:?}"));
            // Any event outside the allowlist (Enter/Esc, releases, modifier state) after the frame's last preedit leaves no composition behind.
            let changes_caret = |e: &Event| match e {
                Event::Text(_) | Event::Paste(_) | Event::Cut => true,
                Event::Key { key, pressed: true, .. } => !matches!(key, Key::Enter | Key::Escape),
                _ => false,
            };
            let is_preedit = |e: &Event| matches!(e, Event::Ime(ImeEvent::Preedit { text, .. }) if !text.is_empty());
            let last_change = events.iter().rposition(changes_caret);
            let last_preedit = events.iter().rposition(is_preedit);
            let must_be_cleared = last_change.is_some_and(|c| last_preedit.is_none_or(|p| c > p));
            let carries = !carried_after(&pending, &events).is_empty();
            // After a reset, a frame of IME events alone (no editing, and nothing carried in) must leave every character
            // of the new text.
            let only_ime = pending.is_empty()
                && events.iter().all(|e| matches!(e, Event::Ime(ImeEvent::Preedit { .. } | ImeEvent::Commit(_))));
            pending = carried_after(&pending, &events);
            h.input_mut().events.extend(events);
            h.run_steps(1);
            if reset && only_ime {
                let kept: String = h.state().text.chars().filter(|c| !"漢字".contains(*c)).collect();
                assert_eq!(
                    kept,
                    "wxyz",
                    "round {round}: an old selection survived a reset: {:?} after {log:#?}",
                    h.state().text
                );
            }
            // A preedit that waits for the next frame takes the events after it along.
            if must_be_cleared && !carries {
                let spoken = shown(&h, "Search");
                assert!(
                    !PREEDITS.iter().any(|p| spoken.contains(p)),
                    "round {round}: a composition survived a caret-changing event: {spoken:?} after {log:#?}"
                );
            }
            let text = &h.state().text;
            assert!(
                !PREEDITS.iter().any(|p| text.contains(p)),
                "round {round}: a preedit leaked into the value {text:?} after {log:#?}"
            );
        }
    }
}

// ---- Event order (#822) -------------------------------------------------------------------------------------------

#[test]
fn a_surrounding_delete_then_a_preedit_in_one_frame_apply_in_order() {
    let mut h = text_field("abc");
    one_frame(
        &mut h,
        vec![
            Event::Ime(ImeEvent::DeleteSurrounding { before_chars: 1, after_chars: 0 }),
            Event::Ime(ImeEvent::Preedit { text: "か".to_owned(), active_range_chars: None }),
        ],
    );
    h.run_steps(1);
    assert_eq!(h.state().text, "ab", "the delete lands; the preedit is not in the value");
    assert_eq!(shown(&h, "Search"), "abか", "and the preedit shows where the delete left the caret");
    commit(&mut h, "漢");
    assert_eq!(h.state().text, "ab漢", "the composition kept going and commits at the caret");
}

#[test]
fn typing_then_select_all_then_a_preedit_in_one_frame_apply_in_order() {
    let mut h = text_field("ab");
    let select_all = Event::Key {
        key: Key::A,
        physical_key: Some(Key::A),
        pressed: true,
        repeat: false,
        modifiers: Modifiers::COMMAND,
    };
    one_frame(
        &mut h,
        vec![
            Event::Text("x".to_owned()),
            Event::ModifiersChanged(Modifiers::COMMAND),
            select_all,
            Event::Ime(ImeEvent::Preedit { text: "か".to_owned(), active_range_chars: None }),
        ],
    );
    h.run_steps(1);
    assert_eq!(h.state().text, "abx", "the letter lands and select-all replaces nothing");
    assert_eq!(shown(&h, "Search"), "か", "the preedit stands in for the whole selection");
    preedit(&mut h, "");
    h.run_steps(1);
    assert_eq!(h.state().text, "abx", "a cancel gives the selection back");
    assert_eq!(shown(&h, "Search"), "abx");
}

/// A single-line field by the rules this file sets, event by event: the reference the widget is held to. Selection is
/// `(anchor, caret)` in characters; a composition holds the selection it stands in for and its preedit.
struct Model {
    text: Vec<char>,
    anchor: usize,
    caret: usize,
    composition: Option<((usize, usize), String)>,
    focused: bool,
    carried: Vec<Event>,
}

impl Model {
    fn new(text: &str) -> Self {
        let len = text.chars().count();
        Self {
            text: text.chars().collect(),
            anchor: len,
            caret: len,
            composition: None,
            focused: true,
            carried: vec![],
        }
    }

    fn value(&self) -> String {
        self.text.iter().collect()
    }

    /// What a screen reader reads: the value with the preedit in place of what it stands in for.
    fn shown(&self) -> String {
        match &self.composition {
            None => self.value(),
            Some(((a, b), preedit)) => {
                let (from, to) = ((*a).min(*b), (*a).max(*b));
                self.text[..from]
                    .iter()
                    .chain(preedit.chars().collect::<Vec<_>>().iter())
                    .chain(&self.text[to..])
                    .collect()
            }
        }
    }

    fn sorted(&self) -> (usize, usize) {
        (self.anchor.min(self.caret), self.anchor.max(self.caret))
    }

    fn collapse(&mut self, at: usize) {
        (self.anchor, self.caret) = (at, at);
    }

    /// Ends the composition; one over a selection gives the selection back.
    fn end(&mut self) {
        if let Some(((a, b), _)) = self.composition.take() {
            (self.anchor, self.caret) = (a, b);
        }
    }

    fn replace_selection(&mut self, with: &str) {
        let (from, to) = self.sorted();
        self.text.splice(from..to, with.chars());
        self.collapse(from + with.chars().count());
    }

    /// One frame: the events carried from the last one, then `events`.
    fn frame(&mut self, events: Vec<Event>) {
        let queue: Vec<Event> = std::mem::take(&mut self.carried).into_iter().chain(events).collect();
        let (mut ime_turn, mut pushed) = (false, false);
        for (index, event) in queue.iter().enumerate() {
            if !self.focused {
                return;
            }
            let composing = self.composition.is_some();
            // Ending a composition over a selection after the editor acted at its caret waits a frame.
            let over_selection = self.composition.as_ref().is_some_and(|((a, b), _)| a != b);
            let ends = match event {
                Event::Ime(ImeEvent::Commit(_)) | Event::Text(_) | Event::Paste(_) | Event::Cut => true,
                Event::Ime(ImeEvent::Preedit { text, .. }) => text.is_empty(),
                Event::Key { key, pressed: true, .. } => !matches!(key, Key::Enter | Key::Escape),
                _ => false,
            };
            if pushed && over_selection && ends {
                self.carried = queue[index..].to_vec();
                return;
            }
            match event {
                Event::Ime(ImeEvent::Preedit { text, .. }) if !text.is_empty() => {
                    // A new composition waits for the caret the edits before it leave.
                    if pushed && !composing {
                        self.carried = queue[index..].to_vec();
                        return;
                    }
                    let held = if let Some((held, _)) = self.composition.take() {
                        held
                    } else {
                        // The selection is held aside and the caret waits at its start.
                        let held = (self.anchor, self.caret);
                        self.collapse(self.sorted().0);
                        held
                    };
                    self.composition = Some((held, text.clone()));
                    ime_turn = true;
                }
                Event::Ime(ImeEvent::Preedit { text, .. } | ImeEvent::Commit(text))
                    if text.is_empty() && !composing => {}
                Event::Ime(ImeEvent::Commit(text)) => {
                    self.end();
                    ime_turn = true;
                    if !text.is_empty() {
                        self.replace_selection(text);
                        pushed = true;
                    }
                }
                Event::Ime(ImeEvent::Preedit { .. }) => {
                    self.end();
                    ime_turn = true;
                }
                Event::Ime(ImeEvent::DeleteSurrounding { before_chars, after_chars }) => {
                    // Around the caret, which a composition holds at its start.
                    let (from, to) = self.sorted();
                    let after = (*after_chars).min(self.text.len() - to);
                    self.text.drain(to..to + after);
                    let before = (*before_chars).min(from);
                    self.text.drain(from - before..from);
                    // egui moves both ends back by the whole count, each stopping at the start.
                    self.anchor = self.anchor.saturating_sub(*before_chars);
                    self.caret = self.caret.saturating_sub(*before_chars);
                    if let Some(((a, b), _)) = self.composition.as_mut() {
                        let (start, width) = ((*a).min(*b), (*a).max(*b) - (*a).min(*b));
                        let (start, width) = (start.saturating_sub(*before_chars), width - (*after_chars).min(width));
                        (*a, *b) = if a <= b { (start, start + width) } else { (start + width, start) };
                    }
                    pushed = true;
                }
                Event::Key { key: Key::Enter, pressed: true, .. } if composing || ime_turn => {}
                Event::Key { key: Key::Enter, pressed: true, .. } => {
                    // A single-line field gives the keyboard up on Enter, and the rest of the frame goes elsewhere.
                    self.focused = false;
                    return;
                }
                Event::Text(text) | Event::Paste(text) => {
                    self.end();
                    self.replace_selection(text);
                    pushed = true;
                }
                Event::Cut => {
                    self.end();
                    let (from, to) = self.sorted();
                    if from != to {
                        self.replace_selection("");
                    }
                    pushed = true;
                }
                Event::Key { key, pressed: true, modifiers, .. } => {
                    self.end();
                    self.key(*key, *modifiers);
                    pushed = true;
                }
                _ => {}
            }
        }
    }

    /// The keys the property test presses, as egui's single-line `TextEdit` does them.
    fn key(&mut self, key: Key, modifiers: Modifiers) {
        let (from, to) = self.sorted();
        let len = self.text.len();
        match key {
            Key::A if modifiers.command => (self.anchor, self.caret) = (0, len),
            Key::Backspace if from != to => self.replace_selection(""),
            Key::Backspace => {
                if from > 0 {
                    self.text.remove(from - 1);
                    self.collapse(from - 1);
                }
            }
            Key::ArrowLeft if modifiers.is_none() && from != to => self.collapse(from),
            Key::ArrowRight if modifiers.is_none() && from != to => self.collapse(to),
            Key::ArrowLeft | Key::ArrowRight | Key::Home | Key::End => {
                self.caret = match key {
                    Key::ArrowLeft => self.caret.saturating_sub(1),
                    Key::ArrowRight => (self.caret + 1).min(len),
                    Key::Home => 0,
                    _ => len,
                };
                if !modifiers.shift {
                    self.anchor = self.caret;
                }
            }
            _ => {}
        }
    }
}

/// The field holds exactly what the reference model holds after every frame, in any order of preedits, commits,
/// typing, platform deletions, caret moves, selection, select-all, cut, paste and Enter, batched or not (#822).
#[test]
fn the_value_matches_a_reference_model_in_any_ordering() {
    const PREEDITS: [&str; 3] = ["か", "き", "く"];
    let mut seed =
        std::env::var("IME_PROPERTY_SEED").ok().and_then(|v| v.parse().ok()).unwrap_or(0x9e37_79b9_7f4a_7c15_u64);
    let mut next = move |bound: usize| {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        usize::try_from(seed % u64::try_from(bound).unwrap_or(1)).unwrap_or(0)
    };
    let key = |key: Key, modifiers: Modifiers| Event::Key {
        key,
        physical_key: Some(key),
        pressed: true,
        repeat: false,
        modifiers,
    };
    let rounds = std::env::var("IME_PROPERTY_ROUNDS").ok().and_then(|v| v.parse().ok()).unwrap_or(400);
    for round in 0..rounds {
        let mut h = text_field("ab");
        let mut model = Model::new("ab");
        let mut log = Vec::new();
        for frame in 0..=8 {
            let mut events = Vec::new();
            // The last frame is empty, so what a preedit carried is applied before the final comparison.
            for _ in 0..if frame == 8 { 0 } else { next(4) + 1 } {
                events.push(match next(13) {
                    0 | 1 => {
                        Event::Ime(ImeEvent::Preedit { text: PREEDITS[next(3)].to_owned(), active_range_chars: None })
                    }
                    2 => empty_preedit(),
                    3 => Event::Ime(ImeEvent::Commit(["漢", "字", ""][next(3)].to_owned())),
                    4 => Event::Text(["x", "y"][next(2)].to_owned()),
                    5 => Event::Ime(ImeEvent::DeleteSurrounding { before_chars: next(2) + 1, after_chars: next(2) }),
                    6 => key([Key::ArrowLeft, Key::ArrowRight, Key::Home, Key::End][next(4)], Modifiers::NONE),
                    7 => key(Key::Backspace, Modifiers::NONE),
                    8 => key(Key::A, Modifiers::COMMAND),
                    9 => key([Key::ArrowLeft, Key::ArrowRight, Key::Home, Key::End][next(4)], Modifiers::SHIFT),
                    10 => Event::Cut,
                    11 => Event::Paste(["p", "qr"][next(2)].to_owned()),
                    _ => key(Key::Enter, Modifiers::NONE),
                });
            }
            log.push(format!("{events:?}"));
            model.frame(events.clone());
            h.input_mut().events.extend(events);
            h.run_steps(1);
            let field = h.state().text.clone();
            assert_eq!(field, model.value(), "round {round}: the value differs from the model after {log:#?}");
            if model.focused {
                assert_eq!(
                    shown(&h, "Search"),
                    model.shown(),
                    "round {round}: what shows differs from the model after {log:#?}"
                );
            }
        }
    }
}

/// What a frame of `events`, after `pending`, can carry into the next one: everything from the first preedit, or the
/// first event that ends a composition, that follows an edit. Counts every key press, commit and platform delete as an
/// edit, so it may say more is carried than is, never less.
fn carried_after(pending: &[Event], events: &[Event]) -> Vec<Event> {
    let queue: Vec<Event> = pending.iter().chain(events).cloned().collect();
    let edit = |e: &Event| {
        matches!(
            e,
            Event::Text(_)
                | Event::Paste(_)
                | Event::Cut
                | Event::Key { pressed: true, .. }
                | Event::Ime(ImeEvent::Commit(_) | ImeEvent::DeleteSurrounding { .. })
        )
    };
    let waits = |e: &Event| {
        matches!(e, Event::Ime(ImeEvent::Preedit { .. } | ImeEvent::Commit(_)))
            || matches!(e, Event::Text(_) | Event::Paste(_) | Event::Cut | Event::Key { pressed: true, .. })
    };
    let first_edit = queue.iter().position(edit);
    first_edit
        .and_then(|from| queue.iter().skip(from + 1).position(waits).map(|at| from + 1 + at))
        .map_or_else(Vec::new, |at| queue[at..].to_vec())
}

#[test]
fn moving_the_caret_during_a_composition_does_not_change_which_text_is_preedit() {
    let mut h = text_field("ab");
    preedit(&mut h, "か");
    // The caret moves (a click, a platform move) without the composition ending, and Home is the IME's key.
    select(&mut h, 0, 0);
    assert_eq!(h.state().text, "ab", "the caret is not the preedit");
    press(&mut h, Modifiers::NONE, Key::Home);
    assert_eq!(h.state().text, "ab");
    preedit(&mut h, "き");
    assert_eq!(h.state().text, "ab", "a new preedit replaces the old one, wherever the caret went");
    commit(&mut h, "漢");
    assert_eq!(h.state().text, "漢ab", "the caret move ended the first composition; the next one commits at the caret");
}

#[test]
fn undo_after_a_composition_never_restores_a_preedit() {
    let mut h = text_field("ab");
    preedit(&mut h, "か");
    // Long enough for the editor to take its own undo point.
    let events = vec![Event::Ime(ImeEvent::Preedit { text: "かん".to_owned(), active_range_chars: None })];
    h.input_mut().time = Some(2.5);
    one_frame(&mut h, events);
    h.input_mut().time = Some(5.0);
    commit(&mut h, "漢");
    assert_eq!(h.state().text, "ab漢");
    h.input_mut().time = Some(8.0);
    one_frame(
        &mut h,
        vec![
            Event::ModifiersChanged(Modifiers::COMMAND),
            Event::Key {
                key: Key::Z,
                physical_key: Some(Key::Z),
                pressed: true,
                repeat: false,
                modifiers: Modifiers::COMMAND,
            },
        ],
    );
    h.run_steps(2);
    assert!(
        !["か", "き", "く"].iter().any(|p| h.state().text.contains(p)),
        "undo went to a state with a preedit in it: {:?}",
        h.state().text
    );
}

#[test]
fn pasting_during_a_composition_ends_it_and_pastes_into_the_committed_text() {
    let mut h = text_field("ab");
    preedit(&mut h, "か");
    one_frame(&mut h, vec![Event::Paste("hello".to_owned())]);
    assert_eq!(h.state().text, "abhello", "the paste lands whole; the preedit is not in the value");
    assert!(!shown(&h, "Search").contains('か'), "and the composition is over: {}", shown(&h, "Search"));
}

#[test]
fn select_all_undo_and_a_click_end_the_composition_in_their_own_frame() {
    let key = |key: Key, modifiers: Modifiers| Event::Key {
        key,
        physical_key: Some(key),
        pressed: true,
        repeat: false,
        modifiers,
    };
    for (what, events) in [
        ("select-all", vec![Event::ModifiersChanged(Modifiers::COMMAND), key(Key::A, Modifiers::COMMAND)]),
        ("undo", vec![Event::ModifiersChanged(Modifiers::COMMAND), key(Key::Z, Modifiers::COMMAND)]),
        ("redo", vec![Event::ModifiersChanged(Modifiers::COMMAND), key(Key::Y, Modifiers::COMMAND)]),
        (
            "redo (shift)",
            vec![
                Event::ModifiersChanged(Modifiers::COMMAND),
                key(Key::Z, Modifiers { shift: true, ..Modifiers::COMMAND }),
            ],
        ),
    ] {
        let mut h = text_field("ab");
        preedit(&mut h, "か");
        assert!(shown(&h, "Search").contains('か'));
        one_frame(&mut h, events);
        assert!(!shown(&h, "Search").contains('か'), "{what} left the preedit showing: {}", shown(&h, "Search"));
        assert_eq!(h.state().text.replace('か', ""), h.state().text);
    }
    let mut h = text_field("ab");
    preedit(&mut h, "か");
    h.get_by_role_and_label(Role::TextInput, "Search").click();
    h.run_steps(1);
    assert!(!shown(&h, "Search").contains('か'), "a click left the preedit showing: {}", shown(&h, "Search"));
}

#[test]
fn an_idle_empty_commit_is_noise_and_leaves_enter_and_escape_alone() {
    let mut h = text_field("ab");
    ime(&mut h, ImeEvent::Commit(String::new()));
    press(&mut h, Modifiers::NONE, Key::Enter);
    assert_eq!(h.state().submitted, 1, "an idle empty commit does not take the Enter");
    // The same for an empty preedit and for Esc, which clears the field when nothing composes.
    let mut h = text_field("ab");
    ime(&mut h, ImeEvent::Preedit { text: String::new(), active_range_chars: None });
    ime(&mut h, ImeEvent::Commit(String::new()));
    press(&mut h, Modifiers::NONE, Key::Escape);
    assert_eq!(h.state().text, "", "idle empty notifications do not take the Esc: it clears as usual");
}

#[test]
fn the_candidate_window_follows_the_preedit_end_even_over_a_selection() {
    let mut caret = text_field("abcdef");
    select(&mut caret, 1, 1);
    preedit(&mut caret, "か");
    let mut selected = text_field("abcdef");
    select(&mut selected, 1, 5);
    preedit(&mut selected, "か");
    let (plain, over) = (cursor_x(&caret), cursor_x(&selected));
    assert!(plain.is_some() && over.is_some());
    assert!(
        (plain.unwrap_or_default() - over.unwrap_or_default()).abs() < 0.5,
        "the rectangle sits at the preedit end whatever is selected: {plain:?} vs {over:?}"
    );
}

/// The cancellation rule is an allowlist: every key but Enter and Escape ends a composition and then does what it does
/// without one. A table over every `egui::Key` and the modifier sets that change what a key means.
#[test]
fn every_key_ends_a_composition_and_then_acts_as_it_would_without_one() {
    let mac_cmd = Modifiers { mac_cmd: true, command: true, ..Modifiers::NONE };
    let sets = [
        ("none", Modifiers::NONE),
        ("Ctrl", Modifiers::CTRL),
        ("Ctrl+Shift", Modifiers { shift: true, ..Modifiers::CTRL }),
        ("Alt", Modifiers::ALT),
        ("Cmd", mac_cmd),
    ];
    for key in egui::Key::ALL.iter().copied().filter(|k| !matches!(k, Key::Enter | Key::Escape)) {
        for (name, modifiers) in sets {
            let event = || {
                vec![
                    Event::ModifiersChanged(modifiers),
                    Event::Key { key, physical_key: Some(key), pressed: true, repeat: false, modifiers },
                ]
            };
            let mut composing = text_field("ab");
            preedit(&mut composing, "か");
            one_frame(&mut composing, event());
            let mut plain = text_field("ab");
            one_frame(&mut plain, event());
            assert!(
                !shown(&composing, "Search").contains('か'),
                "{name}+{key:?} left the preedit showing: {}",
                shown(&composing, "Search")
            );
            assert_eq!(
                composing.state().text,
                plain.state().text,
                "{name}+{key:?} did something else with a composition than without"
            );
        }
    }
}

#[test]
fn enter_and_escape_keep_their_ime_behaviour_in_the_allowlist() {
    for key in [Key::Enter, Key::Escape] {
        let mut h = text_field("ab");
        preedit(&mut h, "か");
        press(&mut h, Modifiers::NONE, key);
        assert!(shown(&h, "Search").contains('か'), "{key:?} must not end the composition");
        assert_eq!(h.state().submitted, 0);
        assert_eq!(h.state().text, "ab");
    }
}

/// A preedit that starts over a selection stands in for it: the text is untouched, a commit replaces the selection
/// as one undo step, a cancel shows it again, and the candidate window sits at the end of the painted preedit.
fn composition_over_a_selection(select_from: usize, select_to: usize) {
    let mut h = text_field("abcdef");
    select(&mut h, select_from, select_to);
    preedit(&mut h, "か");
    assert_eq!(shown(&h, "Search"), "aかf", "the preedit is drawn in place of the selection");
    assert_eq!(h.state().text, "abcdef", "the text itself is untouched");
    // The same text and preedit position without a selection paint the same, so the rectangle must match it.
    let mut plain = text_field("af");
    select(&mut plain, 1, 1);
    preedit(&mut plain, "か");
    assert_eq!(shown(&plain, "Search"), "aかf");
    let (over, at_caret) = (cursor_x(&h), cursor_x(&plain));
    assert!(
        over.zip(at_caret).is_some_and(|(a, b)| (a - b).abs() < 0.5),
        "the candidate rectangle is at the painted preedit end: {over:?} vs {at_caret:?}"
    );
    // Cancel: the selection is shown again, and typing replaces it as it would have.
    let mut cancelled = text_field("abcdef");
    select(&mut cancelled, select_from, select_to);
    preedit(&mut cancelled, "か");
    preedit(&mut cancelled, "");
    cancelled.run_steps(2);
    assert_eq!(shown(&cancelled, "Search"), "abcdef");
    one_frame(&mut cancelled, vec![Event::Text("z".to_owned())]);
    assert_eq!(cancelled.state().text, "azf", "the selection 1..5 is back, so typing replaces it");
    // Commit: the selection is replaced, in one undo step.
    h.input_mut().time = Some(10.0);
    commit(&mut h, "か");
    assert_eq!(h.state().text, "aかf");
    h.input_mut().time = Some(20.0);
    one_frame(
        &mut h,
        vec![
            Event::ModifiersChanged(Modifiers::COMMAND),
            Event::Key {
                key: Key::Z,
                physical_key: Some(Key::Z),
                pressed: true,
                repeat: false,
                modifiers: Modifiers::COMMAND,
            },
        ],
    );
    h.run_steps(2);
    assert_eq!(h.state().text, "abcdef", "one undo gives the text from before the commit");
}

/// An owner that replaces the text under a composition over a selection owns the new text: the old selection is not
/// given back into it, so a later commit lands at the caret and deletes nothing of the new text.
fn reset_under_a_composition_over_a_selection(select_from: usize, select_to: usize) {
    let mut h = text_field("abcdef");
    select(&mut h, select_from, select_to);
    preedit(&mut h, "か");
    "wxyz".clone_into(&mut h.state_mut().text);
    h.run_steps(1);
    commit(&mut h, "漢");
    assert_eq!(without(&h.state().text, '漢'), "wxyz", "nothing of the new text is replaced: {:?}", h.state().text);
    assert!(h.state().text.contains('漢'));
}

#[test]
fn a_reset_under_a_composition_with_no_selection_keeps_all_of_the_new_text() {
    let mut h = text_field("abcdef");
    select(&mut h, 3, 3);
    preedit(&mut h, "か");
    "wxyz".clone_into(&mut h.state_mut().text);
    h.run_steps(1);
    commit(&mut h, "漢");
    assert_eq!(without(&h.state().text, '漢'), "wxyz");
    assert!(h.state().text.contains('漢'));
}

#[test]
fn a_reset_under_a_forward_selection_composition_keeps_all_of_the_new_text() {
    reset_under_a_composition_over_a_selection(1, 5);
}

#[test]
fn a_reset_under_a_backwards_selection_composition_keeps_all_of_the_new_text() {
    reset_under_a_composition_over_a_selection(5, 1);
}

#[test]
fn a_composition_over_a_selection_stands_in_for_it() {
    composition_over_a_selection(1, 5);
}

#[test]
fn a_composition_over_a_backwards_selection_stands_in_for_it() {
    composition_over_a_selection(5, 1);
}

// ---- TreeRow rename -----------------------------------------------------------------------------------------------

#[derive(Default)]
struct Rename {
    name: String,
    renamed: Vec<String>,
    cancelled: u32,
    /// Like an app: the rename ends when it is committed or cancelled.
    done: bool,
}

fn rename() -> Harness<'static, Rename> {
    let mut h = harness_state(
        Settings::default(),
        vec2(360.0, 80.0),
        Rename { name: "Hair".to_owned(), ..Rename::default() },
        |ui, s| {
            let row = TreeRow::new(1, "Hair", icons::CUBE);
            let out = if s.done { row.show(ui) } else { row.renaming(&mut s.name).show(ui) };
            s.done |= out.renamed.is_some() || out.rename_cancelled;
            s.renamed.extend(out.renamed);
            s.cancelled += u32::from(out.rename_cancelled);
        },
    );
    h.run_steps(3);
    h
}

#[test]
fn a_rename_composes_commits_and_leaves_enter_and_escape_to_the_ime() {
    let mut h = rename();
    assert!(h.get_by_role_and_label(Role::TextInput, "Rename Hair").is_focused());
    preedit(&mut h, "かみ");
    assert!(shown(&h, "Rename Hair").contains("かみ"), "the preedit shows: {}", shown(&h, "Rename Hair"));
    assert!(cursor_x(&h).is_some());
    press(&mut h, Modifiers::NONE, Key::Enter);
    press(&mut h, Modifiers::NONE, Key::Escape);
    assert!(h.state().renamed.is_empty(), "Enter during composition renames nothing: {:?}", h.state().renamed);
    assert_eq!(h.state().cancelled, 0, "and Esc does not cancel");
    commit(&mut h, "髪");
    assert_eq!(h.state().renamed.len(), 0, "a commit alone renames nothing");
    press(&mut h, Modifiers::NONE, Key::Enter);
    assert_eq!(h.state().renamed.len(), 1, "the next Enter renames");
    assert!(h.state().renamed[0].ends_with('髪') && !h.state().renamed[0].contains('か'), "{:?}", h.state().renamed);
}

#[test]
fn cancelling_a_rename_composition_over_the_selected_name_keeps_the_name() {
    let mut h = rename();
    press(&mut h, Modifiers::COMMAND, Key::A);
    preedit(&mut h, "か");
    preedit(&mut h, "");
    press(&mut h, Modifiers::NONE, Key::Enter);
    assert_eq!(h.state().renamed, ["Hair"], "the name is as it was, not emptied by the cancelled preedit");
}

// ---- ColourField hex and channel entry ----------------------------------------------------------------------------

struct Colours {
    colour: Colour,
    events: Vec<ColourEvent>,
}

fn colour() -> Harness<'static, Colours> {
    let mut h = harness_state(
        Settings::default(),
        vec2(360.0, 460.0),
        Colours { colour: Colour::rgb(0x8A, 0x6F, 0xD1), events: Vec::new() },
        |ui, s| {
            s.events.extend(ColourField::new("Hair root colour", &mut s.colour).show(ui).event);
        },
    );
    h.get_by_role_and_label(Role::ColorWell, "Hair root colour").click();
    h.run_steps(3);
    h
}

fn open(h: &Harness<'_, Colours>) -> bool {
    h.query_by_role_and_label(Role::Slider, "Hue").is_some()
}

#[test]
fn the_hex_field_composes_and_enter_or_escape_do_not_close_the_popover_meanwhile() {
    let mut h = colour();
    h.get_by_role_and_label(Role::TextInput, "Hex colour").focus();
    h.run_steps(2);
    let original = h.state().colour;
    preedit(&mut h, "１１");
    assert_eq!(h.state().colour, original, "a preedit is not a colour yet");
    press(&mut h, Modifiers::NONE, Key::Enter);
    assert!(open(&h), "Enter belongs to the IME: the popover stays open");
    press(&mut h, Modifiers::NONE, Key::Escape);
    assert!(open(&h), "and so does Esc");
    assert_eq!(h.state().colour, original, "and nothing was cancelled or committed");
    commit(&mut h, "112233");
    assert_eq!(h.state().colour, Colour::rgb(0x11, 0x22, 0x33), "a commit previews like typing");
    press(&mut h, Modifiers::NONE, Key::Enter);
    assert!(!open(&h), "the next Enter commits and closes");
}

#[test]
fn a_colour_channel_field_takes_full_width_digits_and_enter_waits_for_the_ime() {
    let mut h = colour();
    // The channel boxes are the text inputs showing the bytes; red is 0x8A = 138.
    let red = h.get_all_by_role(Role::TextInput).find(|n| n.value().as_deref() == Some("138"));
    red.expect("a channel box for red").focus();
    h.run_steps(2);
    channel_scenario(&mut h);
}

#[test]
fn a_colour_channel_field_rejects_other_text_with_its_own_error_and_keeps_the_popover() {
    let mut h = colour();
    let red = h.get_all_by_role(Role::TextInput).find(|n| n.value().as_deref() == Some("138"));
    red.expect("a channel box for red").focus();
    h.run_steps(2);
    press(&mut h, Modifiers::COMMAND, Key::A);
    commit(&mut h, "あ");
    press(&mut h, Modifiers::NONE, Key::Enter);
    assert!(open(&h), "the popover stays open over text that is not a number");
    assert_eq!(h.state().colour.r, 0x8A, "and the colour is untouched");
    assert!(h.query_by_label_contains("Enter a number").is_some(), "the field's own message shows");
}

#[test]
fn a_colour_channel_keeps_rejected_text_and_its_error_when_the_box_is_left() {
    let mut h = colour();
    let red = h.get_all_by_role(Role::TextInput).find(|n| n.value().as_deref() == Some("138"));
    red.expect("a channel box for red").focus();
    h.run_steps(2);
    press(&mut h, Modifiers::COMMAND, Key::A);
    commit(&mut h, "あ");
    h.get_by_role_and_label(Role::TextInput, "Hex colour").focus();
    h.run_steps(3);
    assert!(open(&h), "the popover stays open");
    assert_eq!(h.state().colour.r, 0x8A, "and the colour is untouched");
    assert!(h.query_by_label_contains("Enter a number").is_some(), "the box keeps its message after blur");
    assert!(h.get_all_by_role(Role::TextInput).any(|n| n.value().as_deref() == Some("あ")), "and the rejected text");
}

fn channel_scenario(h: &mut Harness<'_, Colours>) {
    press(h, Modifiers::COMMAND, Key::A);
    preedit(h, "２");
    press(h, Modifiers::NONE, Key::Enter);
    assert_eq!(h.state().colour.r, 0x8A, "Enter during composition commits nothing");
    commit(h, "２００");
    press(h, Modifiers::NONE, Key::Enter);
    assert_eq!(h.state().colour.r, 200, "full-width 200 reads as 200");
}
