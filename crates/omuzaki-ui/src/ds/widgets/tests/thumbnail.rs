//! Thumbnail tiles: one named, selectable stop; a preview on hover and focus; a texture asked for only when visible.

use egui::{TextureId, Ui};
use egui_kittest::Harness;

use super::*;
use crate::ds::testing::unlabelled_interactive;

fn texture() -> Thumb {
    Thumb::Ready(TextureId::Managed(1))
}

struct Rig {
    thumb: Thumb,
    selected: bool,
    clicked: u32,
    last: Option<(bool, bool, bool)>,
}

fn rig(thumb: Thumb) -> Harness<'static, Rig> {
    harness_state(
        Settings::default(),
        vec2(360.0, 220.0),
        Rig { thumb, selected: false, clicked: 0, last: None },
        |ui: &mut Ui, r| {
            ui.add(Button::new("Before"));
            let out = ThumbnailTile::new("Oval", r.thumb).selected(r.selected).show(ui);
            r.clicked += u32::from(out.response.clicked());
            r.last = Some((out.on_screen, out.previewing, out.wants_texture));
        },
    )
}

#[test]
fn a_tile_is_a_named_button_with_a_selected_state_and_one_tab_stop() {
    let mut h = rig(texture());
    let node = h.get_by_role_and_label(Role::Button, "Oval");
    assert_eq!(node.accesskit_node().toggled(), Some(egui::accesskit::Toggled::False));
    h.state_mut().selected = true;
    h.run();
    assert_eq!(
        h.get_by_role_and_label(Role::Button, "Oval").accesskit_node().toggled(),
        Some(egui::accesskit::Toggled::True)
    );
    h.get_by_role_and_label(Role::Button, "Before").focus();
    h.run();
    press(&mut h, Modifiers::NONE, Key::Tab);
    assert!(h.get_by_role_and_label(Role::Button, "Oval").is_focused());
    press(&mut h, Modifiers::NONE, Key::Enter);
    assert_eq!(h.state().clicked, 1, "Enter chooses the focused tile");
    assert_eq!(unlabelled_interactive(&h), Vec::<String>::new());
}

#[test]
fn hovering_or_focusing_a_tile_asks_for_a_preview_and_leaving_takes_it_back() {
    let mut h = rig(texture());
    assert_eq!(h.state().last.map(|l| l.1), Some(false));
    h.get_by_role_and_label(Role::Button, "Oval").hover();
    h.run_steps(2);
    assert_eq!(h.state().last.map(|l| l.1), Some(true), "the pointer previews it");
    assert_eq!(
        h.get_by_role_and_label(Role::Button, "Oval").accesskit_node().description().as_deref(),
        Some("Previewing")
    );
    h.hover_at(egui::pos2(300.0, 200.0));
    h.run_steps(2);
    assert_eq!(h.state().last.map(|l| l.1), Some(false));
    h.get_by_role_and_label(Role::Button, "Oval").focus();
    h.run();
    assert_eq!(h.state().last.map(|l| l.1), Some(true), "keyboard focus previews it too");
}

#[test]
fn a_tile_without_its_picture_says_so_and_asks_for_it_only_while_it_is_on_screen() {
    let mut h = rig(Thumb::Loading);
    assert_eq!(h.state().last, Some((true, false, true)), "on screen and loading: load it");
    assert_eq!(
        h.get_by_role_and_label(Role::Button, "Oval").accesskit_node().description().as_deref(),
        Some("Loading preview")
    );
    h.state_mut().thumb = texture();
    h.run();
    assert_eq!(h.state().last.map(|l| l.2), Some(false), "once it has a picture nothing is wanted");
    h.state_mut().thumb = Thumb::Missing;
    h.run();
    assert_eq!(
        h.get_by_role_and_label(Role::Button, "Oval").accesskit_node().description().as_deref(),
        Some("No preview")
    );
    assert!(crate::ds::audit::painted(h.output()).iter().any(|p| p.text == "No preview"), "the word is painted too");
    assert_eq!(h.state().last.map(|l| l.2), Some(false), "a failed one is not asked for again");

    // The same tile far below the window: not on screen, so not wanted.
    let far = harness_state(Settings::default(), vec2(200.0, 100.0), None::<bool>, |ui: &mut Ui, wants| {
        ui.add_space(2_000.0);
        *wants = Some(ThumbnailTile::new("Oval", Thumb::Loading).show(ui).wants_texture);
    });
    assert_eq!(*far.state(), Some(false), "a tile nobody can see loads nothing");
}

#[test]
fn a_disabled_tile_says_why_does_not_choose_and_does_not_preview() {
    let mut h = harness_state(Settings::default(), vec2(360.0, 200.0), (0u32, false), |ui: &mut Ui, s| {
        let out = ThumbnailTile::new("Oval", Thumb::Loading).disabled("Locked by Designer").show(ui);
        s.0 += u32::from(out.response.clicked());
        s.1 |= out.previewing;
    });
    let node = h.get_by_role_and_label(Role::Button, "Oval");
    assert!(node.accesskit_node().is_disabled());
    assert_eq!(node.accesskit_node().description().as_deref(), Some("Locked by Designer"));
    node.click();
    h.run();
    h.get_by_role_and_label(Role::Button, "Oval").hover();
    h.run_steps(2);
    assert_eq!(*h.state(), (0, false));
}
