//! The shortcut sheet (`shell.md` section 16, Ctrl+/ or Help, Keyboard shortcuts): the global keys the studio answers
//! today, each drawn as key caps beside what it does. Esc, a click outside or Close dismisses it.

use egui::{Context, Id, Key, KeyboardShortcut, Modal, Modifiers, RichText, Sense, WidgetInfo, WidgetType, vec2};

use super::text::{Word, say};
use crate::ds::theme::ds;
use crate::ds::tokens::space;
use crate::ds::widgets::keycap::{CapStyle, Shortcut};
use crate::ds::widgets::{Button, KeyCaps};

/// Width of the sheet's content.
const WIDTH: f32 = 380.0;

/// The keys the sheet lists, in the order of `shell.md` section 16, and what each does.
const KEYS: [(Modifiers, Key, Word); 11] = [
    (Modifiers::COMMAND, Key::K, Word::SheetPalette),
    (Modifiers::COMMAND, Key::Z, Word::SheetUndo),
    (Modifiers::COMMAND.plus(Modifiers::SHIFT), Key::Z, Word::SheetRedo),
    (Modifiers::COMMAND, Key::Num1, Word::SheetWorkspaces),
    (Modifiers::NONE, Key::F6, Word::SheetPanels),
    (Modifiers::COMMAND, Key::Space, Word::SheetMaximise),
    (Modifiers::COMMAND.plus(Modifiers::SHIFT), Key::L, Word::SheetLock),
    (Modifiers::COMMAND.plus(Modifiers::ALT), Key::P, Word::SheetPause),
    (Modifiers::NONE, Key::ArrowRight, Word::SheetDivider),
    (Modifiers::COMMAND, Key::Slash, Word::SheetSheet),
    (Modifiers::COMMAND, Key::Comma, Word::SheetSettings),
];

/// Draws the sheet; false once the person dismissed it.
pub fn show(ctx: &Context) -> bool {
    let ds = ds(ctx);
    let mut close = false;
    let frame = egui::Frame::popup(&ctx.global_style()).inner_margin(egui::Margin::same(16));
    let modal = Modal::new(Id::new("shell_shortcut_sheet")).frame(frame).show(ctx, |ui| {
        ui.set_width(WIDTH);
        ui.label(RichText::new(say(ctx, Word::SheetTitle, &[])).font(ds.text.title.clone()));
        ui.add_space(space::S8);
        let t = &ds.theme;
        let style = CapStyle { fill: t.bg3, stroke: t.line, text: t.text2 };
        for (modifiers, key, word) in KEYS {
            let what = say(ctx, word, &[]);
            let caps = KeyCaps::new(Shortcut::new(modifiers, key));
            let spoken = ctx.format_shortcut(&KeyboardShortcut::new(modifiers, key));
            ui.horizontal(|ui| {
                ui.label(&what);
                let width = caps.width(ctx);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let (rect, response) = ui.allocate_exact_size(vec2(width, 20.0), Sense::hover());
                    caps.paint(ui.painter(), &ds, rect.left_center(), style);
                    response.widget_info(|| WidgetInfo::labeled(WidgetType::Label, true, &spoken));
                });
            });
        }
        ui.add_space(space::S12);
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            close = ui.add(Button::new(&say(ctx, Word::SheetClose, &[]))).clicked();
        });
    });
    !(close || modal.should_close())
}
