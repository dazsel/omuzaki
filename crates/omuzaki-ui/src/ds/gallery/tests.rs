//! Gallery-wide checks: every story is registered once, and in every theme, scale and density the whole gallery draws
//! with every interactive node named for a screen reader (`design-system.md` section 10).

use std::collections::HashSet;

use egui::vec2;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;

use super::{Gallery, all};
use crate::ds::audit::{contrast_findings, painted, plain_english};
use crate::ds::testing::{
    accesskit_strings, all_strings, reading_order_violations, tab_through, unlabelled_interactive,
};
use crate::ds::theme::Settings;
use crate::ds::tokens::{Density, Scale, Theme, ThemeKind};

/// A window tall enough to show every card of the gallery in three columns, and in one: a widget outside the window is
/// not drawn, so a shorter one would leave the stories below it out of every check here.
const WIDE_HEIGHT: f32 = 6_500.0;
const COLUMN_HEIGHT: f32 = 20_000.0;

#[test]
fn every_component_has_one_story_with_a_unique_id() {
    let stories = all();
    let ids: HashSet<_> = stories.iter().map(|s| s.id).collect();
    assert_eq!(ids.len(), stories.len(), "duplicate story ids");
    for needed in [
        "button",
        "toggle",
        "segmented",
        "param_slider",
        "colour_field",
        "tree",
        "toast",
        "approval",
        "channel_meter",
        "dropdown",
        "tab",
        "dialog",
        "form_row",
    ] {
        assert!(ids.contains(needed), "no story for {needed}");
    }
}

fn gallery_harness(settings: Settings) -> Harness<'static, Gallery> {
    let mut harness = Harness::builder()
        .with_size(vec2(1680.0, WIDE_HEIGHT))
        .build_ui_state(|ui, gallery: &mut Gallery| gallery.ui(ui), Gallery::new(settings));
    harness.run_steps(6);
    harness
}

#[test]
fn every_interactive_node_of_every_story_has_an_accessible_label() {
    for theme in ThemeKind::ALL {
        let harness = gallery_harness(Settings { theme, ..Settings::default() });
        let bad = unlabelled_interactive(&harness);
        assert!(bad.is_empty(), "{theme:?}: unlabelled interactive nodes:\n{}", bad.join("\n"));
    }
}

#[test]
fn the_gallery_draws_at_every_scale_and_density_in_every_theme() {
    for theme in ThemeKind::ALL {
        for scale in [75, 100, 200] {
            for density in Density::ALL {
                let mut harness =
                    gallery_harness(Settings { theme, scale: Scale::nearest(scale), density, reduce_motion: false });
                harness.run_steps(2);
                assert!(harness.state().content_height() > 500.0, "{theme:?} {scale}% {density:?} drew nothing");
            }
        }
    }
}

#[test]
fn reduce_motion_draws_the_whole_gallery_too() {
    let harness = gallery_harness(Settings { reduce_motion: true, ..Settings::default() });
    assert_eq!(unlabelled_interactive(&harness), Vec::<String>::new());
}

#[test]
fn every_painted_text_meets_its_contrast_in_every_theme() {
    // The measured twin of the contrast table: the real frame, each text against what is painted under it.
    for theme in ThemeKind::ALL {
        let harness = gallery_harness(Settings { theme, ..Settings::default() });
        let findings = contrast_findings(harness.output(), Theme::of(theme));
        assert!(
            findings.is_empty(),
            "{theme:?}: {} texts miss their contrast:\n{}",
            findings.len(),
            findings.iter().map(ToString::to_string).collect::<Vec<_>>().join("\n")
        );
    }
}

/// The gallery in one column (a window too narrow for two), tall enough to show every card without scrolling.
fn one_column(settings: Settings) -> Harness<'static, Gallery> {
    let mut harness = Harness::builder()
        .with_size(vec2(520.0, COLUMN_HEIGHT))
        .build_ui_state(|ui, gallery: &mut Gallery| gallery.ui(ui), Gallery::new(settings));
    harness.run_steps(6);
    harness
}

#[test]
fn tab_order_is_reading_order_and_every_stop_shows_a_focus_indicator() {
    for theme in [ThemeKind::Dark, ThemeKind::HighContrast] {
        let mut harness = one_column(Settings { theme, ..Settings::default() });
        let stops = tab_through(&mut harness, 400);
        assert!(stops.len() > 100, "{theme:?}: only {} tab stops; the gallery has about two hundred", stops.len());
        let order = reading_order_violations(&stops);
        assert!(order.is_empty(), "{theme:?}: Tab does not follow the eye:\n{}", order.join("\n"));
        let unseen: Vec<String> = stops
            .iter()
            .filter(|s| !s.indicated)
            .map(|s| format!("{:?} '{}' at {:?}", s.role, s.label, s.rect))
            .collect();
        assert!(unseen.is_empty(), "{theme:?}: focus is invisible on:\n{}", unseen.join("\n"));
        let unnamed: Vec<_> = stops.iter().filter(|s| s.label.trim().is_empty()).collect();
        assert!(unnamed.is_empty(), "{theme:?}: focusable but unnamed: {unnamed:?}");
        let offscreen: Vec<_> =
            stops.iter().filter(|s| s.rect.top() < 0.0 || s.rect.bottom() > COLUMN_HEIGHT).collect();
        assert!(offscreen.is_empty(), "{theme:?}: focus lands out of view: {offscreen:?}");
    }
}

#[test]
fn the_pseudo_locale_gallery_has_no_plain_english_from_a_component_or_a_story() {
    for theme in ThemeKind::ALL {
        let mut harness = Harness::builder().with_size(vec2(1680.0, WIDE_HEIGHT)).build_ui_state(
            |ui, gallery: &mut Gallery| gallery.ui(ui),
            Gallery::new(Settings { theme, ..Settings::default() }).with_pseudo(true),
        );
        harness.run_steps(6);
        let strings = all_strings(&harness);
        assert!(strings.len() > 300, "the scan saw only {} strings", strings.len());
        let english = plain_english(strings.iter().map(String::as_str));
        assert!(english.is_empty(), "{theme:?}: these came through as plain English:\n{}", english.join("\n"));
    }
}

#[test]
fn the_pseudo_locale_really_changes_the_gallery() {
    let plain = all_strings(&gallery_harness(Settings::default()));
    assert!(plain_english(plain.iter().map(String::as_str)).len() > 50, "English has plenty of plain English in it");
    assert!(plain.iter().any(|s| s == "Approve for this session"), "and the components say their own words in it");
}

// ---- The approval gallery's scope explanations ---------------------------------------------------------------------
// These drive the gallery's own cards, so a change to the wording or wiring of the story is what they catch.

const INPUT_MEANING: &str = "Lets this read your keyboard, mouse and gamepad, even while another program is in front.";
const PROCESS_MEANING: &str =
    "Lets this start named programs on this computer and check whether a named program is already running.";

/// How many painted texts of the frame contain `part`.
fn painted_count(harness: &Harness<'_, Gallery>, part: &str) -> usize {
    painted(harness.output()).iter().filter(|p| p.text.contains(part)).count()
}

#[test]
fn the_gallery_explains_input_and_process_inline_and_to_a_screen_reader_without_hover() {
    let harness = gallery_harness(Settings::default());
    for meaning in [INPUT_MEANING, PROCESS_MEANING] {
        assert_eq!(
            painted_count(&harness, meaning),
            1,
            "the card paints its explanation once, without hover: {meaning}"
        );
        assert!(
            accesskit_strings(&harness).iter().any(|s| s.contains(meaning)),
            "the card's accessible text carries its explanation: {meaning}"
        );
    }
}

#[test]
fn hovering_a_scope_badge_adds_the_tooltip_to_the_inline_explanation() {
    for (scope, meaning) in [("input", INPUT_MEANING), ("process", PROCESS_MEANING)] {
        let mut harness = gallery_harness(Settings::default());
        harness.get_by_role_and_label(egui::accesskit::Role::Label, scope).hover();
        harness.run_steps(10);
        assert_eq!(painted_count(&harness, meaning), 2, "{scope}: the tooltip paints it beside the inline line");
    }
}
