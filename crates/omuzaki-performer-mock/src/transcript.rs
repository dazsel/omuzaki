//! [`Transcript`]: the golden-transcript writer and differ (`ai-performer.md` §8, §9).

use omuzaki_performer_api::event::{PerformerEvent, TranscriptLine};
use omuzaki_performer_api::ids::PersonaRevision;

/// A transcript under construction: JSON Lines, one [`TranscriptLine`] per event, stamped with one persona revision.
#[derive(Clone, Debug)]
pub struct Transcript {
    persona: PersonaRevision,
    lines: Vec<TranscriptLine>,
}

impl Transcript {
    /// An empty transcript for `persona`.
    pub fn new(persona: PersonaRevision) -> Self {
        Self { persona, lines: Vec::new() }
    }

    /// Records `event` at `t_ms` (private events lose their text, [`TranscriptLine::new`]).
    pub fn push(&mut self, t_ms: u64, event: PerformerEvent) {
        self.lines.push(TranscriptLine::new(t_ms, self.persona.clone(), event));
    }

    /// The lines so far.
    pub fn lines(&self) -> &[TranscriptLine] {
        &self.lines
    }

    /// The transcript as JSON Lines, each line ending in `\n`.
    ///
    /// # Errors
    /// Only if a number is not finite.
    pub fn to_jsonl(&self) -> Result<String, String> {
        let mut out = String::new();
        for line in &self.lines {
            out.push_str(&line.to_json().map_err(|e| format!("line at t={}: {e}", line.t))?);
            out.push('\n');
        }
        Ok(out)
    }

    /// Compares this transcript with a golden file's text, byte for byte.
    ///
    /// # Errors
    /// The first line that differs (counted from 1), with both versions, or a difference in length.
    pub fn diff(&self, golden: &str) -> Result<(), String> {
        let actual = self.to_jsonl()?;
        if actual == golden {
            return Ok(());
        }
        let mut got = actual.lines();
        let mut want = golden.lines();
        for n in 1.. {
            match (got.next(), want.next()) {
                (Some(g), Some(w)) if g == w => {}
                (g, w) => {
                    return Err(format!(
                        "line {n}:\n  got:  {}\n  want: {}",
                        g.unwrap_or("<end>"),
                        w.unwrap_or("<end>")
                    ));
                }
            }
        }
        Err("the transcripts differ only in their line endings".into())
    }
}
