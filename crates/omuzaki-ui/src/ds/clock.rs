//! The clock widgets measure time with (`design-system.md` section 9, rule 1).
//!
//! Timers in the UI (a toast's lifetime, the pause before a dangerous request can be confirmed) are measured against a
//! clock kept in the egui context, never by adding up frame times: a reactive app repaints rarely, so `stable_dt` after
//! a sleep says nothing about how long the sleep was. The default clock is egui's frame time, the monotonic seconds
//! the integration stamps on every frame. An app that already owns a [`Clock`] installs it with [`set_clock`], and a
//! test installs a [`FakeClock`](omuzaki_core::time::FakeClock) and moves it by hand, so a four second toast is tested
//! without waiting four seconds.

use std::sync::Arc;

use egui::{Context, Id};
use omuzaki_core::time::Clock;

fn clock_id() -> Id {
    Id::new("omuzaki_ui::ds::clock")
}

/// Makes `clock` the time source of every widget in this context.
pub fn set_clock(ctx: &Context, clock: Arc<dyn Clock>) {
    ctx.data_mut(|d| d.insert_temp(clock_id(), clock));
}

/// Seconds on the installed clock, or egui's frame time when none was installed.
pub fn now(ctx: &Context) -> f64 {
    match ctx.data(|d| d.get_temp::<Arc<dyn Clock>>(clock_id())) {
        Some(clock) => clock.now().0,
        None => ctx.input(|i| i.time),
    }
}

#[cfg(test)]
mod tests {
    use omuzaki_core::time::{FakeClock, Seconds};

    use super::*;

    #[test]
    fn the_installed_clock_wins_over_frame_time() {
        let ctx = Context::default();
        let fake = Arc::new(FakeClock::new(Seconds(100.0)));
        set_clock(&ctx, Arc::clone(&fake) as Arc<dyn Clock>);
        assert!((now(&ctx) - 100.0).abs() < f64::EPSILON);
        fake.advance(Seconds(4.0));
        assert!((now(&ctx) - 104.0).abs() < f64::EPSILON);
    }

    #[test]
    fn without_one_the_clock_is_egui_frame_time() {
        let ctx = Context::default();
        let input = egui::RawInput { time: Some(12.5), ..egui::RawInput::default() };
        ctx.run_ui(input, |ui| assert!((now(ui.ctx()) - 12.5).abs() < f64::EPSILON)).drop_without_applying_deltas();
    }
}
