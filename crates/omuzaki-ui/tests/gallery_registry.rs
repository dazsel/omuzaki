//! The gallery lists the stories `omuzaki_ui_api::registry` holds, in its order, with no list of its own (#2042): a story
//! registered by another crate appears and renders, and every story the design system had before the move is still there.

use egui::vec2;
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable as _;
use omuzaki_ui::ds::Settings;
use omuzaki_ui::ds::gallery::{Gallery, Group, all};
use omuzaki_ui_api::register_story;

// Between the Display stories `chips` (order 10) and `tree` (order 20), and before them both when ties are broken by id.
register_story! { id: "test.between", title: "Registered by a test crate", group: Group::Display, order: 15,
state: u32, show: |ui, n| { *n += 1; ui.label("a story from another crate"); } }

/// The stories the gallery listed on main before it read the registry, in its order (section, then `order`, then id).
/// New stories may join anywhere; none of these may be missing or moved.
const BEFORE: [&str; 23] = [
    "palette",
    "type",
    "button",
    "toggle",
    "segmented",
    "dropdown",
    "param_slider",
    "text_field",
    "colour_field",
    "tab",
    "form_row",
    "curve_editor",
    "chips",
    "tree",
    "channel_meter",
    "thumbnail",
    "step_card",
    "toast",
    "banner_empty",
    "approval",
    "dialog",
    "job_row",
    "inspector",
];

#[test]
fn every_story_the_gallery_had_is_still_listed_in_its_old_order_and_a_new_one_joins_by_its_group_and_order() {
    let ids: Vec<&str> = all().iter().map(|s| s.id).collect();
    let mut at = 0;
    for id in BEFORE {
        let Some(found) = ids[at..].iter().position(|listed| *listed == id) else {
            panic!("story {id:?} is missing or out of order in {ids:?}");
        };
        at += found + 1;
    }
    let (chips, between, tree) = (position(&ids, "chips"), position(&ids, "test.between"), position(&ids, "tree"));
    assert!(chips < between && between < tree, "{ids:?}");
}

fn position(ids: &[&str], id: &str) -> usize {
    ids.iter().position(|listed| *listed == id).unwrap_or_else(|| panic!("{id} is not listed in {ids:?}"))
}

#[test]
fn the_order_is_the_same_every_time() {
    let first: Vec<&str> = all().iter().map(|s| s.id).collect();
    let second: Vec<&str> = all().iter().map(|s| s.id).collect();
    assert_eq!(first, second);
}

#[test]
fn a_story_from_another_crate_renders_in_the_gallery() {
    let mut harness = Harness::builder()
        .with_size(vec2(1680.0, 6_500.0))
        .build_ui_state(|ui, gallery: &mut Gallery| gallery.ui(ui), Gallery::new(Settings::default()));
    harness.run_steps(6);
    assert!(harness.query_by_label("Registered by a test crate").is_some(), "the card heading");
    assert!(harness.query_by_label("a story from another crate").is_some(), "the story's body");
}
