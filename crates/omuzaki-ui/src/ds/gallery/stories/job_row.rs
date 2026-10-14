//! Feedback: `JobRow`, in each state.

use egui::Ui;

use crate::ds::gallery::Group;
use crate::ds::gallery::layout::caption;
use crate::ds::gallery::text::{tr, translate};
use crate::ds::theme::ds;
use crate::ds::widgets::{JobRow, JobState};
use crate::register_story;

#[derive(Default)]
struct State {
    last: Option<&'static str>,
}

register_story! {
    id: "job_row", title: "Job row", group: Group::Feedback, order: 50, state: State,
    show: show,
}

fn show(ui: &mut Ui, s: &mut State) {
    let ds = ds(ui.ctx());
    let ds = &*ds;
    let error = translate(ds, "GPU out of memory. Close the viewport preview or lower the sheet size, then retry.");
    ui.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 8.0;
        // Each row is shown in the statement that builds it: the labels it borrows live that long.
        let mut show = |row: JobRow<'_>| {
            let out = row.show(ui);
            if out.cancel {
                s.last = Some("Cancel was pressed.");
            }
            if out.retry {
                s.last = Some("Retry was pressed.");
            }
        };
        show(
            JobRow::new(tr!(ds, "Render 24 contact sheets"), JobState::Running { progress: Some(0.4) })
                .cost(&["~40 s", "1.2 GB", "$0.12"])
                .elapsed("0:17")
                .id_salt("running"),
        );
        show(
            JobRow::new(tr!(ds, "Bake hair physics"), JobState::Running { progress: None })
                .cost(&["~120 s"])
                .elapsed("0:05")
                .id_salt("unknown"),
        );
        show(
            JobRow::new(tr!(ds, "Export the character"), JobState::Queued).cost(&["~15 s", "300 MB"]).id_salt("queued"),
        );
        show(
            JobRow::new(tr!(ds, "Render the likeness turntable"), JobState::Failed { error: &error })
                .cost(&["~40 s", "1.2 GB"])
                .elapsed("0:22")
                .id_salt("failed"),
        );
        show(JobRow::new(tr!(ds, "Generate the expression strip"), JobState::Done).elapsed("1:12").id_salt("done"));
    });
    ui.add_space(8.0);
    caption(
        ui,
        ds,
        s.last.unwrap_or("A running job always has Cancel and its cost; a failed one shows the error and Retry."),
    );
    caption(
        ui,
        ds,
        "With no known fraction the bar is hatched and still: nothing loops, and an idle list costs no frames.",
    );
}
