//! Display: `ThumbnailTile`, with a picture, a placeholder, a failure, a selection and the lazy-loading contract.

use egui::Ui;

use super::sample_art;
use crate::ds::gallery::Group;
use crate::ds::gallery::layout::caption;
use crate::ds::gallery::text::translate;
use crate::ds::theme::ds;
use crate::ds::widgets::{Thumb, ThumbnailTile};
use omuzaki_ui_api::register_story;

#[derive(Default)]
struct State {
    chosen: usize,
    previewing: Option<usize>,
    waiting: usize,
}

register_story! {
    id: "thumbnail", title: "Thumbnail tile", group: Group::Display, order: 40, state: State,
    show: show,
}

fn show(ui: &mut Ui, s: &mut State) {
    let ds = ds(ui.ctx());
    let ds = &*ds;
    let pictures = [0u8, 1, 2].map(|v| sample_art::texture(ui.ctx(), &ds.theme, v));
    let names = ["Oval", "Round", "Heart", "Square", "Soft", "Locked"].map(|n| translate(ds, n));
    let tiles = [
        (&names[0], Thumb::Ready(pictures[0])),
        (&names[1], Thumb::Ready(pictures[1])),
        (&names[2], Thumb::Ready(pictures[2])),
        (&names[3], Thumb::Loading),
        (&names[4], Thumb::Missing),
        (&names[5], Thumb::Ready(pictures[0])),
    ];
    let locked = translate(ds, "Locked by Designer");
    let mut waiting = 0;
    let mut previewing = None;
    ui.horizontal_wrapped(|ui| {
        ui.spacing_mut().item_spacing = egui::vec2(12.0, 8.0);
        for (i, (name, thumb)) in tiles.iter().enumerate() {
            let mut tile = ThumbnailTile::new(name, *thumb).selected(s.chosen == i).id_salt(("tile", i));
            if i == 5 {
                tile = tile.disabled(&locked);
            }
            let out = tile.show(ui);
            if out.response.clicked() {
                s.chosen = i;
            }
            waiting += usize::from(out.wants_texture);
            if out.previewing {
                previewing = Some(i);
            }
        }
    });
    s.waiting = waiting;
    s.previewing = previewing;
    ui.add_space(8.0);
    let note = match s.previewing {
        Some(_) => "Previewing: the host applies this choice to the viewport until the pointer leaves.",
        None => "Hover or focus a tile to preview it. A tile on screen with no picture yet asks the host to load one.",
    };
    caption(ui, ds, note);
    caption(ui, ds, if s.waiting > 0 { "One tile is waiting for its picture." } else { "Nothing is waiting." });
}
