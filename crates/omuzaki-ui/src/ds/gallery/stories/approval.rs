//! Feedback: `ApprovalCard`, in each variant.

use egui::Ui;

use crate::ds::gallery::Group;
use crate::ds::gallery::layout::caption;
use crate::ds::gallery::text::{tr, translate};
use crate::ds::theme::ds;
use crate::ds::widgets::{ApprovalCard, ApprovalVariant, Decision, PlanStep};
use omuzaki_ui_api::register_story;

#[derive(Default)]
struct State {
    last: Option<Decision>,
}

register_story! {
    id: "approval", title: "Approval card", group: Group::Feedback, order: 30, state: State,
    show: show,
}

fn show(ui: &mut Ui, s: &mut State) {
    let ds = ds(ui.ctx());
    let ds = &*ds;
    let (palette, sheets, publish) = (
        translate(ds, "Set the hair colour palette"),
        translate(ds, "Render 24 contact sheets"),
        translate(ds, "Publish the preview to the portfolio"),
    );
    let steps = [
        PlanStep { label: &palette, in_scope: true },
        PlanStep { label: &sheets, in_scope: true },
        PlanStep { label: &publish, in_scope: false },
    ];
    let (round, almond, sharp) =
        (translate(ds, "Round eyes"), translate(ds, "Almond eyes"), translate(ds, "Sharp eyes"));
    let options = [&*round, &*almond, &*sharp];
    let designer = translate(ds, "Designer");
    let confirm = format!("{}: {}", translate(ds, "Confirm"), translate(ds, "go live as @omuzakidev"));
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 12.0;
        let decisions = [
            ApprovalCard::new("render", &designer, tr!(ds, "Render 24 contact sheets to check expressions"))
                .scope("render")
                .scope_meaning(tr!(ds, "Allows rendering previews. Doesn't change the project."))
                .cost(&["~40 s", "1.2 GB GPU", "$0"])
                .reversible(true)
                .changes(tr!(ds, "nothing in the character"), &[tr!(ds, "no document changes")])
                .similar(6)
                .show(ui),
            ApprovalCard::new("plan", &designer, tr!(ds, "Make the hair softer and check likeness"))
                .scope("edit:character")
                .variant(ApprovalVariant::Plan(&steps))
                .show(ui),
            ApprovalCard::new("ask", &designer, tr!(ds, "Which eye shape fits her best?"))
                .scope("read")
                .variant(ApprovalVariant::Question(&options))
                .show(ui),
            ApprovalCard::new("live", &designer, tr!(ds, "Go live on Twitch as @omuzakidev"))
                .scope("broadcast")
                .variant(ApprovalVariant::Dangerous { confirm: &confirm })
                .reversible(false)
                .show(ui),
            ApprovalCard::new("input", &designer, tr!(ds, "Watch keys and clicks while the stream app has focus"))
                .scope("input")
                .scope_meaning(tr!(ds, "Lets this read your keyboard, mouse and gamepad, even while another program is in front."))
                .show(ui),
            ApprovalCard::new("process", &designer, tr!(ds, "Start OBS and check it is running"))
                .scope("process")
                .scope_meaning(tr!(ds, "Lets this start named programs on this computer and check whether a named program is already running."))
                .show(ui),
        ];
        if let Some(d) = decisions.into_iter().flatten().next() {
            s.last = Some(d);
        }
    });
    ui.add_space(8.0);
    let text = s
        .last
        .as_ref()
        .map_or_else(|| "Decide on a card to see the result.".to_owned(), |d| format!("Decision: {d:?}"));
    caption(ui, ds, &text);
}
