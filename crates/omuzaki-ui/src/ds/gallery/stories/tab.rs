//! Controls: `TabBar`, as workspace tabs and as dock tabs.

use egui::{Margin, Ui, vec2};

use crate::ds::gallery::Group;
use crate::ds::gallery::layout::{caption, specimen};
use crate::ds::gallery::text::{tr, translate};
use crate::ds::icons;
use crate::ds::theme::ds;
use crate::ds::tokens::radius;
use crate::ds::widgets::{TabBar, TabItem, TabKind};
use crate::register_story;

struct State {
    workspace: usize,
    narrow: usize,
    icons: usize,
    dock: usize,
    closed: Option<usize>,
}

impl Default for State {
    fn default() -> Self {
        Self { workspace: 1, narrow: 1, icons: 1, dock: 0, closed: None }
    }
}

register_story! {
    id: "tab", title: "Tabs", group: Group::Controls, order: 70, state: State,
    show: show,
}

fn show(ui: &mut Ui, s: &mut State) {
    let ds = ds(ui.ctx());
    let ds = &*ds;
    let bar = |ui: &mut Ui, add: &mut dyn FnMut(&mut Ui)| {
        egui::Frame::new().fill(ds.theme.bg2).corner_radius(radius::CARD).inner_margin(Margin::symmetric(4, 0)).show(
            ui,
            |ui| {
                ui.set_width(ui.available_width());
                add(ui);
            },
        );
    };
    let names = ["Build", "Face", "Rig", "Look", "Perform", "Theatre"].map(|n| translate(ds, n));
    specimen(ui, ds, "Workspace", |ui| {
        bar(ui, &mut |ui| {
            let tabs = [
                TabItem::new(&names[0]),
                TabItem::new(&names[1]),
                TabItem::new(&names[2]),
                TabItem::new(&names[3]),
                TabItem::new(&names[4]),
                TabItem::new(&names[5]).agent_active(true),
            ];
            ui.add(TabBar::new(tr!(ds, "Workspaces"), &mut s.workspace, &tabs).id_salt("tabs_workspace"));
        });
    });
    specimen(ui, ds, "Icons only", |ui| {
        bar(ui, &mut |ui| {
            let tabs = [
                TabItem::new(&names[0]).icon(icons::CUBE).icon_only(),
                TabItem::new(&names[1]).icon(icons::SMILEY).icon_only(),
                TabItem::new(&names[2]).icon(icons::SLIDERS_HORIZONTAL).icon_only(),
                TabItem::new(&names[5]).icon(icons::ROBOT).icon_only().agent_active(true),
            ];
            ui.add(TabBar::new(tr!(ds, "Compact workspaces"), &mut s.icons, &tabs).id_salt("tabs_icons"));
        });
    });
    specimen(ui, ds, "Dock", |ui| {
        bar(ui, &mut |ui| {
            let (outliner, library, history) =
                (translate(ds, "Outliner"), translate(ds, "Library"), translate(ds, "History"));
            let tabs = [
                TabItem::new(&outliner).icon(icons::TREE_STRUCTURE),
                TabItem::new(&library).icon(icons::FOLDER).closable(),
                TabItem::new(&history).icon(icons::CLOCK).closable().agent_active(true),
            ];
            let out =
                TabBar::new(tr!(ds, "Left dock"), &mut s.dock, &tabs).kind(TabKind::Dock).id_salt("tabs_dock").show(ui);
            if out.closed.is_some() {
                s.closed = out.closed;
            }
        });
    });
    specimen(ui, ds, "Too narrow", |ui| {
        ui.allocate_ui(vec2(170.0, 40.0), |ui| {
            bar(ui, &mut |ui| {
                let tabs =
                    [TabItem::new(&names[0]), TabItem::new(&names[1]).agent_active(true), TabItem::new(&names[4])];
                ui.add(TabBar::new(tr!(ds, "Narrow workspaces"), &mut s.narrow, &tabs).id_salt("tabs_narrow"));
            });
        });
    });
    let note = s.closed.map_or(
        "Left and Right move and select; Delete or a middle click closes a dock tab.",
        |_| "A dock tab asked to close: the host removes it.",
    );
    caption(ui, ds, note);
}
