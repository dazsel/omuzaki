//! Feedback: Toast.

use egui::Ui;

use crate::ds::gallery::Group;
use crate::ds::gallery::layout::{caption, specimen};
use crate::ds::gallery::text::{tr, translate};
use crate::ds::theme::ds;
use crate::ds::tokens::Status;
use crate::ds::widgets::{Button, Toast, ToastCard, Toasts};
use crate::register_story;

#[derive(Default)]
struct State {
    toasts: Toasts,
    pushed: u32,
}

register_story! {
    id: "toast", title: "Toast", group: Group::Feedback, order: 10, state: State,
    show: show,
}

fn show(ui: &mut Ui, s: &mut State) {
    let ds = ds(ui.ctx());
    let ds = &*ds;
    let say = |text| translate(ds, text).into_owned();
    let ok = Toast::new(Status::Ok, say("Designer changed the face profile")).action(say("Undo")).action(say("Show"));
    let info = Toast::new(Status::Info, say("Autosaved 14:32"));
    let warn =
        Toast::new(Status::Warn, say("Tracking confidence is low. Move closer to the camera.")).action(say("Details"));
    let error =
        Toast::new(Status::Danger, say("Render failed: out of GPU memory")).action(say("Retry")).action(say("Details"));
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 8.0;
        ToastCard::new(&ok).show(ui);
        ToastCard::new(&info).show(ui);
        ToastCard::new(&warn).count(3).show(ui);
        ToastCard::new(&error).show(ui);
    });
    ui.add_space(8.0);
    specimen(ui, ds, "Try it", |ui| {
        if ui.add(Button::new(tr!(ds, "Show a toast"))).clicked() {
            s.pushed += 1;
            let ctx = ui.ctx().clone();
            let message = format!("{} 0.{}", say("Designer set Width to"), 40 + s.pushed % 5);
            s.toasts.push(&ctx, Toast::new(Status::Ok, message).action(say("Undo")));
        }
    });
    let ctx = ui.ctx().clone();
    s.toasts.show(&ctx, egui::vec2(16.0, 40.0));
    caption(
        ui,
        ds,
        "Bottom right above the status bar. Success and info go after 4 s, warnings 8 s, errors stay. Timers pause on hover.",
    );
}
