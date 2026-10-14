//! Display: `StepCard`, in each state, and compact.

use egui::Ui;

use super::sample_art;
use crate::ds::gallery::Group;
use crate::ds::gallery::layout::caption;
use crate::ds::gallery::text::{tr, translate};
use crate::ds::theme::ds;
use crate::ds::widgets::{ActorKind, Change, Metric, StepAction, StepCard, StepState, Thumb};
use crate::register_story;

#[derive(Default)]
struct State {
    last: Option<StepAction>,
}

register_story! {
    id: "step_card", title: "Step card", group: Group::Display, order: 50, state: State,
    show: show,
}

fn show(ui: &mut Ui, s: &mut State) {
    let ds = ds(ui.ctx());
    let ds = &*ds;
    let art = [0u8, 1, 2].map(|v| Thumb::Ready(sample_art::texture(ui.ctx(), &ds.theme, v)));
    let (likeness, spacing, mouth) = (translate(ds, "likeness"), translate(ds, "spacing"), translate(ds, "mouth"));
    let applied_metrics = [
        Metric { name: &likeness, delta: "+0.03", change: Change::Better },
        Metric { name: &spacing, delta: "-0.01", change: Change::Better },
        Metric { name: &mouth, delta: "+0.00", change: Change::Neutral },
    ];
    let failed_metrics = [Metric { name: &likeness, delta: "-0.09", change: Change::Worse }];
    let designer = translate(ds, "Designer");
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 10.0;
        let mut show = |card: StepCard<'_>| {
            if let Some(action) = card.show(ui).action {
                s.last = Some(action);
            }
        };
        show(
            StepCard::new(tr!(ds, "Rounder face profile"), &designer, ActorKind::Agent)
                .cause(tr!(ds, "\"make the face more oval\""))
                .metrics(&applied_metrics)
                .age("4 s")
                .pictures(art[0], art[1])
                .id_salt("applied"),
        );
        show(
            StepCard::new(tr!(ds, "Softer fringe"), &designer, ActorKind::Agent)
                .cause(tr!(ds, "\"soften the fringe a little and keep the length\""))
                .metrics(&applied_metrics[..1])
                .age("1 m")
                .state(StepState::Pending)
                .pictures(art[1], art[2])
                .id_salt("pending"),
        );
        show(
            StepCard::new(tr!(ds, "Sharper eyes"), tr!(ds, "Producer"), ActorKind::Person)
                .cause(tr!(ds, "\"sharper eyes\""))
                .age("3 m")
                .state(StepState::Rejected)
                .pictures(art[0], art[2])
                .id_salt("rejected"),
        );
        show(
            StepCard::new(tr!(ds, "Wider smile"), &designer, ActorKind::Agent)
                .cause(tr!(ds, "\"make her smile wider\""))
                .metrics(&failed_metrics)
                .age("5 m")
                .state(StepState::FailedGate)
                .pictures(art[2], art[0])
                .evidence(art[1])
                .id_salt("failed"),
        );
        show(
            StepCard::new(tr!(ds, "Compact card"), &designer, ActorKind::Agent)
                .cause(tr!(ds, "\"a dense list\""))
                .age("1 h")
                .compact(true)
                .pictures(Thumb::Loading, Thumb::Missing)
                .id_salt("compact"),
        );
    });
    ui.add_space(8.0);
    let note = s.last.map_or_else(
        || "Rewind, Explain and Adjust; a pending step offers Approve and Reject, a rejected one Restore.".to_owned(),
        |a| format!("Pressed: {a:?}"),
    );
    caption(ui, ds, &note);
}
