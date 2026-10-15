//! The four per-message counters (D2.7).

use std::time::Duration;

use omuzaki_agent_api::event::Cap;
use omuzaki_core::time::Seconds;

use crate::config::Caps;

/// What one user message has used so far, against [`Caps`].
#[derive(Debug)]
pub(crate) struct Counters {
    started: Seconds,
    excluded: f64,
    turns: u32,
    tool_calls: u32,
    tokens: u64,
}

impl Counters {
    /// Counters for an answer that starts at `now`.
    pub(crate) fn new(now: Seconds) -> Self {
        Self { started: now, excluded: 0.0, turns: 0, tool_calls: 0, tokens: 0 }
    }

    /// One more LLM request was made.
    pub(crate) fn turn(&mut self) {
        self.turns += 1;
    }

    /// One more tool call ran.
    pub(crate) fn call(&mut self) {
        self.tool_calls += 1;
    }

    /// `tokens` more were used.
    pub(crate) fn tokens(&mut self, tokens: u64) {
        self.tokens = self.tokens.saturating_add(tokens);
    }

    /// Takes `waited` out of the wall time: a person may take ten minutes to decide, and that is not the model's time.
    pub(crate) fn exclude(&mut self, waited: Duration) {
        self.excluded += waited.as_secs_f64();
    }

    /// Takes `seconds` of clock time out of the wall time.
    pub(crate) fn exclude_seconds(&mut self, seconds: f64) {
        self.excluded += seconds.max(0.0);
    }

    /// The tool calls run so far.
    pub(crate) fn tool_calls(&self) -> u32 {
        self.tool_calls
    }

    /// The first cap that has been reached at `now`, in the order of D2.7. A counter that equals its cap has reached it.
    pub(crate) fn reached(&self, caps: &Caps, now: Seconds) -> Option<Cap> {
        let wall = (now.0 - self.started.0 - self.excluded).max(0.0);
        if self.turns >= caps.turns {
            Some(Cap::Turns)
        } else if self.tool_calls >= caps.tool_calls {
            Some(Cap::ToolCalls)
        } else if wall >= f64::from(caps.wall_s) {
            Some(Cap::WallTime)
        } else if self.tokens >= caps.tokens {
            Some(Cap::Tokens)
        } else {
            None
        }
    }
}

/// The note the last turn carries once a cap is reached.
pub(crate) fn limit_note(cap: Cap, caps: &Caps) -> String {
    let what = match cap {
        Cap::Turns => format!("{} turns", caps.turns),
        Cap::ToolCalls => format!("{} tool calls", caps.tool_calls),
        Cap::WallTime => format!("{} s of wall time", caps.wall_s),
        Cap::Tokens => format!("{} tokens", caps.tokens),
        _ => "a limit".to_owned(),
    };
    format!("limit reached ({what}): say what you did and what is left")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn caps() -> Caps {
        Caps { turns: 2, tool_calls: 3, wall_s: 10, tokens: 100 }
    }

    #[test]
    fn each_cap_is_reached_at_exactly_its_number() {
        let mut c = Counters::new(Seconds(0.0));
        assert_eq!(c.reached(&caps(), Seconds(0.0)), None);
        c.turn();
        assert_eq!(c.reached(&caps(), Seconds(0.0)), None);
        c.turn();
        assert_eq!(c.reached(&caps(), Seconds(0.0)), Some(Cap::Turns));

        let mut c = Counters::new(Seconds(0.0));
        c.call();
        c.call();
        assert_eq!(c.reached(&caps(), Seconds(0.0)), None);
        c.call();
        assert_eq!(c.reached(&caps(), Seconds(0.0)), Some(Cap::ToolCalls));

        let mut c = Counters::new(Seconds(0.0));
        c.tokens(99);
        assert_eq!(c.reached(&caps(), Seconds(0.0)), None);
        c.tokens(1);
        assert_eq!(c.reached(&caps(), Seconds(0.0)), Some(Cap::Tokens));

        let c = Counters::new(Seconds(5.0));
        assert_eq!(c.reached(&caps(), Seconds(14.999)), None);
        assert_eq!(c.reached(&caps(), Seconds(15.0)), Some(Cap::WallTime));
    }

    #[test]
    fn an_excluded_wait_does_not_count_toward_wall_time() {
        let mut c = Counters::new(Seconds(0.0));
        c.exclude(Duration::from_secs(100));
        assert_eq!(c.reached(&Caps { wall_s: 600, ..Caps::default() }, Seconds(650.0)), None);
        assert_eq!(c.reached(&Caps { wall_s: 600, ..Caps::default() }, Seconds(700.0)), Some(Cap::WallTime));
    }

    #[test]
    fn the_note_names_the_cap_from_the_constants() {
        let note = limit_note(Cap::ToolCalls, &Caps::default());
        assert_eq!(note, "limit reached (60 tool calls): say what you did and what is left");
    }
}
