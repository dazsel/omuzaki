//! Tab bars: one tab stop, arrows that move and select, and the ways a dock tab closes.

use egui::{Event, PointerButton};
use egui_kittest::Harness;

use super::*;
use crate::ds::testing::unlabelled_interactive;

struct Rig {
    selected: usize,
    closed: Vec<usize>,
}

fn rig(width: f32) -> Harness<'static, Rig> {
    harness_state(Settings::default(), vec2(width, 120.0), Rig { selected: 1, closed: Vec::new() }, |ui, r| {
        ui.add(Button::new("Before"));
        let tabs = [
            TabItem::new("Outliner"),
            TabItem::new("Library").closable(),
            TabItem::new("History").closable().agent_active(true),
        ];
        let out = TabBar::new("Left dock", &mut r.selected, &tabs).kind(TabKind::Dock).show(ui);
        r.closed.extend(out.closed);
        ui.add(Button::new("After"));
    })
}

fn tab<'h>(h: &'h Harness<'_, Rig>, label: &'h str) -> egui_kittest::Node<'h> {
    h.get_by_role_and_label(Role::Tab, label)
}

#[test]
fn a_tab_bar_is_a_tab_list_of_tabs_with_the_selected_one_marked_and_each_in_its_set() {
    let h = rig(360.0);
    let list = h.get_by_role_and_label(Role::TabList, "Left dock");
    assert_eq!(list.accesskit_node().orientation(), Some(egui::accesskit::Orientation::Horizontal));
    let selected: Vec<_> =
        ["Outliner", "Library", "History"].map(|l| tab(&h, l).accesskit_node().is_selected() == Some(true)).into();
    assert_eq!(selected, [false, true, false]);
    let library = tab(&h, "Library").accesskit_node();
    assert_eq!((library.position_in_set(), library.size_of_set()), (Some(2), Some(3)));
    assert!(inside(tab(&h, "Library").accesskit_node(), Role::TabList), "tabs belong to the list");
    assert_eq!(unlabelled_interactive(&h), Vec::<String>::new());
}

#[test]
fn the_agent_dot_is_also_said_in_words() {
    let h = rig(360.0);
    assert_eq!(tab(&h, "History").accesskit_node().description().as_deref(), Some("an agent is working here"));
    assert_eq!(tab(&h, "Library").accesskit_node().description(), None, "only the busy tab says so");
}

#[test]
fn the_bar_is_one_tab_stop_and_the_arrows_move_and_select() {
    let mut h = rig(360.0);
    h.get_by_role_and_label(Role::Button, "Before").focus();
    h.run();
    press(&mut h, Modifiers::NONE, Key::Tab);
    assert!(tab(&h, "Library").is_focused(), "Tab lands on the selected tab");
    press(&mut h, Modifiers::NONE, Key::ArrowRight);
    assert_eq!(h.state().selected, 2);
    assert!(tab(&h, "History").is_focused(), "the focus follows the selection");
    press(&mut h, Modifiers::NONE, Key::Home);
    assert_eq!(h.state().selected, 0);
    press(&mut h, Modifiers::NONE, Key::End);
    assert_eq!(h.state().selected, 2);
    press(&mut h, Modifiers::NONE, Key::ArrowRight);
    assert_eq!(h.state().selected, 2, "the arrows stop at the ends");
    press(&mut h, Modifiers::NONE, Key::Tab);
    assert!(h.get_by_role_and_label(Role::Button, "After").is_focused(), "Tab leaves the whole bar");
    press(&mut h, Modifiers::SHIFT, Key::Tab);
    assert!(tab(&h, "History").is_focused(), "and Shift+Tab comes back to the selected tab");
}

#[test]
fn a_click_selects_and_the_close_button_a_middle_click_and_delete_all_close_a_closable_tab() {
    let mut h = rig(360.0);
    tab(&h, "Outliner").click();
    h.run();
    assert_eq!(h.state().selected, 0);
    assert_eq!(h.state().closed, Vec::<usize>::new());
    h.get_by_role_and_label(Role::Button, "Close Library").click();
    h.run();
    assert_eq!(h.state().closed, [1], "the close button asks the host to close that tab");
    let at = tab(&h, "History").rect().center();
    h.hover_at(at);
    h.run();
    for pressed in [true, false] {
        h.event(Event::PointerButton { pos: at, button: PointerButton::Middle, pressed, modifiers: Modifiers::NONE });
    }
    h.run();
    assert_eq!(h.state().closed, [1, 2], "a middle click closes the tab under it");
    tab(&h, "Library").click();
    h.run();
    press(&mut h, Modifiers::NONE, Key::Delete);
    assert_eq!(h.state().closed, [1, 2, 1], "Delete closes the focused tab");
    tab(&h, "Outliner").click();
    h.run();
    press(&mut h, Modifiers::NONE, Key::Delete);
    assert_eq!(h.state().closed.len(), 3, "a tab that is not closable ignores Delete");
}

#[test]
fn the_close_button_is_a_named_target_of_at_least_24_points_but_not_a_tab_stop() {
    let h = rig(360.0);
    let close = h.get_by_role_and_label(Role::Button, "Close Library");
    let rect = close.rect();
    assert!(rect.width() >= 24.0 && rect.height() >= 24.0, "{rect:?}");
    assert!(!close.accesskit_node().data().supports_action(egui::accesskit::Action::Focus));
    assert!(close.accesskit_node().data().supports_action(egui::accesskit::Action::Click));
}

#[test]
fn an_icon_only_tab_is_named_and_a_bar_short_of_room_stays_inside_it() {
    let h = harness_state(Settings::default(), vec2(160.0, 80.0), 0usize, |ui, sel| {
        let tabs = [
            TabItem::new("Build").icon(crate::ds::icons::CUBE).icon_only(),
            TabItem::new("Perform the whole show tonight").icon(crate::ds::icons::PLAY),
            TabItem::new("Theatre of operations").agent_active(true),
        ];
        ui.set_max_width(144.0);
        ui.add(TabBar::new("Workspaces", sel, &tabs));
    });
    h.get_by_role_and_label(Role::Tab, "Build");
    let list = h.get_by_role_and_label(Role::TabList, "Workspaces").rect();
    assert!(list.right() <= list.left() + 144.0 + 0.5, "the bar shrank its tabs to fit: {list:?}");
    for label in ["Perform the whole show tonight", "Theatre of operations"] {
        assert!(h.get_by_role_and_label(Role::Tab, label).rect().right() <= list.right() + 0.5);
    }
}

#[test]
fn a_bar_with_no_tabs_is_inert_and_does_not_panic() {
    let h = harness_state(Settings::default(), vec2(200.0, 60.0), 0usize, |ui, sel| {
        let none: [TabItem<'_>; 0] = [];
        let out = TabBar::new("Nothing", sel, &none).show(ui);
        assert_eq!(out.closed, None);
    });
    assert!(h.query_by_role(Role::Tab).is_none());
}
