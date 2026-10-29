//! The timeline: a short's lines placed on the clock, with the face, the shot and the captions in force at every
//! moment, and the measures the reference bar reads (`docs/quality/reference-shorts.md`).
//!
//! A face lands [`FACE_LEAD_S`] before its line's first word, as a performer's face moves just before the voice; it
//! holds until the next line that names a face. A shot cuts at its line's first word and holds the same way. A line's
//! captions are its phrases ([`phrases`]), each on screen while it is said.

use std::collections::BTreeSet;
use std::fmt::Write as _;

use omuzaki_anim_api::Channel;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::def::{CaptionStyle, ShortDef, ShotNumbers};
use crate::error::ShortError;
use crate::voice::{Span, VoiceTrack};

/// How far ahead of its line's first word a face lands, seconds.
pub const FACE_LEAD_S: f32 = 0.06;
/// How long a caption stays after its phrase ends, unless the next one comes first, seconds.
pub const CAPTION_HOLD_S: f32 = 0.25;

/// A face in force over a stretch.
#[derive(Clone, Debug, PartialEq)]
pub struct FaceSpan {
    /// Seconds.
    pub start: f32,
    /// The expression ids, layered in order; empty is the resting face.
    pub ids: Vec<String>,
    /// Channels set on top, resolved.
    pub channels: Vec<(Channel, f32)>,
}

/// A shot in force from `start`.
#[derive(Clone, Debug, PartialEq)]
pub struct ShotSpan {
    /// Seconds.
    pub start: f32,
    /// When the line that set it ends: where a push in reaches its full zoom.
    pub line_end: f32,
    /// The shot.
    pub shot: ShotNumbers,
}

/// A caption on screen.
#[derive(Clone, Debug, PartialEq)]
pub struct CaptionSpan {
    /// Seconds.
    pub start: f32,
    /// Seconds.
    pub end: f32,
    /// What it says, as drawn.
    pub text: String,
    /// How it looks.
    pub style: CaptionStyle,
}

/// A short on the clock.
#[derive(Clone, Debug, PartialEq)]
pub struct Timeline {
    /// Seconds.
    pub duration: f32,
    /// When each line is spoken.
    pub lines: Vec<Span>,
    /// Faces, in time order; the first starts at 0.
    pub faces: Vec<FaceSpan>,
    /// Shots, in time order; the first starts at 0.
    pub shots: Vec<ShotSpan>,
    /// Captions, in time order.
    pub captions: Vec<CaptionSpan>,
}

impl Timeline {
    /// Places `def`'s lines at `voice`'s spans.
    ///
    /// # Errors
    /// [`ShortError::Invalid`] for a channel name that isn't a channel, or a shot that doesn't read.
    pub fn new(def: &ShortDef, voice: &VoiceTrack) -> Result<Self, ShortError> {
        let mut faces = vec![FaceSpan { start: 0.0, ids: Vec::new(), channels: Vec::new() }];
        let mut shots = vec![ShotSpan { start: 0.0, line_end: 0.0, shot: ShotNumbers::default() }];
        let mut captions = Vec::new();
        for (i, (line, span)) in def.lines.iter().zip(&voice.spans).enumerate() {
            if line.face.is_some() || !line.channels.is_empty() {
                let ids = line.face.as_ref().map_or_else(Vec::new, crate::def::FaceCue::ids);
                let channels = line
                    .channels
                    .iter()
                    .map(|(name, value)| {
                        Channel::from_name(name).map(|c| (c, *value)).ok_or_else(|| {
                            ShortError::invalid(format!("line[{i}].channels.{name}"), "there is no such channel")
                        })
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                let start = (span.start - FACE_LEAD_S).max(0.0);
                push_or_replace(&mut faces, FaceSpan { start, ids, channels }, |f| f.start);
            }
            if let Some(cue) = &line.camera {
                let shot = cue.numbers().map_err(|e| e.at(format!("line[{i}].camera")))?;
                push_or_replace(&mut shots, ShotSpan { start: span.start, line_end: span.end, shot }, |s| s.start);
            }
            for beat in line.beats().map_err(|e| e.at(format!("line[{i}].text")))? {
                let at = span.start + beat.at * span.len();
                if let Some(face) = &beat.face {
                    // A beat's face keeps the line's channels.
                    let channels = faces.last().map(|f| f.channels.clone()).unwrap_or_default();
                    let start = (at - FACE_LEAD_S).max(0.0);
                    push_or_replace(&mut faces, FaceSpan { start, ids: face.ids(), channels }, |f| f.start);
                }
                if let Some(cue) = &beat.camera {
                    let shot = cue.numbers().map_err(|e| e.at(format!("line[{i}].text")))?;
                    push_or_replace(&mut shots, ShotSpan { start: at, line_end: span.end, shot }, |s| s.start);
                }
            }
            if def.captions.enabled && line.caption != CaptionStyle::None {
                let spoken = line.spoken();
                let text = line.caption_text.as_deref().unwrap_or(&spoken);
                lay_captions(text, line.caption, def.captions.max_chars, *span, &mut captions);
            }
        }
        // A caption gives way to the next one.
        for i in 1..captions.len() {
            let next = captions[i].start;
            if captions[i - 1].end > next {
                captions[i - 1].end = next;
            }
        }
        Ok(Self { duration: voice.duration, lines: voice.spans.clone(), faces, shots, captions })
    }

    /// The face in force at `t`.
    #[must_use]
    pub fn face_at(&self, t: f32) -> &FaceSpan {
        at(&self.faces, t, |f| f.start)
    }

    /// The index of the shot in force at `t`.
    #[must_use]
    pub fn shot_index_at(&self, t: f32) -> usize {
        self.shots.partition_point(|s| s.start <= t).saturating_sub(1)
    }

    /// The caption on screen at `t`, if any.
    #[must_use]
    pub fn caption_at(&self, t: f32) -> Option<&CaptionSpan> {
        let i = self.captions.partition_point(|c| c.start <= t).checked_sub(1)?;
        self.captions.get(i).filter(|c| t < c.end)
    }

    /// The measures of the reference bar that the timeline alone answers.
    #[must_use]
    pub fn measures(&self) -> Measures {
        let mut changes = BTreeSet::new();
        for f in self.faces.iter().skip(1) {
            changes.insert(millis(f.start));
        }
        for s in self.shots.iter().skip(1) {
            changes.insert(millis(s.start));
        }
        // A face and a shot that change together are one change; so are two within 120 ms.
        let mut merged = 0usize;
        let mut last: Option<i64> = None;
        for t in changes {
            if last.is_none_or(|last| t - last > 120) {
                merged += 1;
                last = Some(t);
            }
        }
        let states: BTreeSet<String> = self.faces.iter().map(face_key).collect();
        let marks: BTreeSet<String> = self
            .faces
            .iter()
            .flat_map(|f| {
                let expressions = f.ids.iter().filter(|id| MARK_EXPRESSIONS.contains(&id.as_str())).cloned();
                let channels = f
                    .channels
                    .iter()
                    .filter(|(c, v)| *v > 0.0 && MARK_CHANNELS.contains(&c.info().name))
                    .map(|(c, _)| c.info().name.to_owned());
                expressions.chain(channels)
            })
            .collect();
        let spoken: f32 = self.lines.iter().map(Span::len).sum();
        let captioned: f32 = self
            .lines
            .iter()
            .map(|line| {
                self.captions.iter().map(|c| (c.end.min(line.end) - c.start.max(line.start)).max(0.0)).sum::<f32>()
            })
            .sum();
        #[allow(clippy::cast_precision_loss, reason = "counts")]
        let per_minute = if self.duration > 0.0 { merged as f32 * 60.0 / self.duration } else { 0.0 };
        Measures {
            duration_s: round2(self.duration),
            lines: self.lines.len(),
            changes: merged,
            changes_per_minute: round2(per_minute),
            distinct_faces: states.len(),
            marks: marks.into_iter().collect(),
            shots: self.shots.len(),
            captions: self.captions.len(),
            caption_coverage: round2(if spoken > 0.0 { (captioned / spoken).min(1.0) } else { 0.0 }),
        }
    }
}

/// Expressions of the built-in library that are anime marks (FE-02) rather than plain moods.
pub const MARK_EXPRESSIONS: [&str; 9] =
    ["flustered", "laugh", "gloom", "dizzy", "hearts", "stars", "sparkle", "sweat", "scared"];
/// Channels that are anime marks, set directly.
pub const MARK_CHANNELS: [&str; 16] = [
    "eyesSqueeze",
    "eyesShock",
    "eyesGloom",
    "eyesSpiral",
    "eyesHeart",
    "eyesStar",
    "sparkle",
    "sweatDrop",
    "tears",
    "angerVein",
    "eyesBlank",
    "eyesDot",
    "eyesPill",
    "teethSharp",
    "eyeArcLeft",
    "eyeArcRight",
];

/// The measures of a short's timeline (reference bar, `docs/quality/reference-shorts.md`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Measures {
    /// Seconds.
    pub duration_s: f32,
    /// How many lines.
    pub lines: usize,
    /// Moments where the face or the shot changes (two within 120 ms count once).
    pub changes: usize,
    /// `changes` per minute: the bar asks for at least 40 in a talking-head short.
    pub changes_per_minute: f32,
    /// Distinct faces (expression sets with their channels).
    pub distinct_faces: usize,
    /// The anime marks used, by expression or channel name.
    pub marks: Vec<String>,
    /// How many shots.
    pub shots: usize,
    /// How many captions.
    pub captions: usize,
    /// The share of spoken time with a caption on screen, 0 to 1.
    pub caption_coverage: f32,
}

fn round2(v: f32) -> f32 {
    (v * 100.0).round() / 100.0
}

#[allow(clippy::cast_possible_truncation, reason = "a short is at most 600 s")]
fn millis(t: f32) -> i64 {
    (t * 1000.0).round() as i64
}

fn face_key(f: &FaceSpan) -> String {
    let mut key = f.ids.join("+");
    for (c, v) in &f.channels {
        let _ = write!(key, "|{}={v:.2}", c.info().name);
    }
    key
}

fn at<T>(spans: &[T], t: f32, start: impl Fn(&T) -> f32) -> &T {
    let i = spans.partition_point(|s| start(s) <= t).saturating_sub(1);
    &spans[i.min(spans.len() - 1)]
}

/// Pushes `span`, or replaces the last one when it starts at the same moment (a first line at the very start).
fn push_or_replace<T>(spans: &mut Vec<T>, span: T, start: impl Fn(&T) -> f32) {
    if spans.last().is_some_and(|last| (start(last) - start(&span)).abs() < 1e-4) {
        spans.pop();
    }
    spans.push(span);
}

/// Splits `text` into the phrases a caption shows: words gathered up to `max_chars`, broken after punctuation once a
/// phrase holds a third of that.
#[must_use]
pub fn phrases(text: &str, max_chars: usize) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    for word in text.split_whitespace() {
        let len = current.chars().count();
        if len > 0 && len + 1 + word.chars().count() > max_chars {
            out.push(std::mem::take(&mut current));
        }
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(word);
        let ends = word.ends_with([',', '.', '!', '?', ';', ':', '…']);
        if ends && current.chars().count() * 3 >= max_chars {
            out.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
    out
}

/// Lays `text`'s phrases over `span`, each for its share of the characters.
#[allow(clippy::cast_precision_loss, reason = "character counts")]
fn lay_captions(text: &str, style: CaptionStyle, max_chars: usize, span: Span, out: &mut Vec<CaptionSpan>) {
    let limit = match style {
        CaptionStyle::Shout => max_chars * 3 / 4,
        CaptionStyle::Big => 12,
        CaptionStyle::Aside => max_chars * 5 / 4,
        CaptionStyle::Normal | CaptionStyle::None => max_chars,
    }
    .max(4);
    let parts = phrases(text, limit);
    let total: usize = parts.iter().map(|p| p.chars().count()).sum();
    if total == 0 {
        return;
    }
    let mut t = span.start;
    for part in parts {
        let len = span.len() * part.chars().count() as f32 / total as f32;
        let shown = if style == CaptionStyle::Shout { part.to_uppercase() } else { part };
        out.push(CaptionSpan { start: t, end: t + len + CAPTION_HOLD_S, text: shown, style });
        t += len;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::voice::voice_for;

    fn timeline(text: &str) -> Timeline {
        let def = ShortDef::from_toml(text).unwrap();
        let voice = voice_for(&def, |_| unreachable!()).unwrap();
        Timeline::new(&def, &voice).unwrap()
    }

    const SHORT: &str = r#"
character = "lumi"
[[line]]
text = "Is this not an aggressive language, really?"
face = "flustered"
camera = "close+tilt-left"
caption = "shout"
[[line]]
text = "No."
[[line]]
text = "Fine."
face = "smug"
camera = "bust"
"#;

    #[test]
    fn faces_and_shots_hold_until_a_line_changes_them() {
        let tl = timeline(SHORT);
        assert_eq!(tl.faces.len(), 3, "rest, flustered, smug");
        assert_eq!(tl.face_at(tl.lines[1].start + 0.01).ids, ["flustered"]);
        assert_eq!(tl.face_at(tl.lines[2].start).ids, ["smug"]);
        assert_eq!(tl.face_at(0.0).ids, Vec::<String>::new());
        let shot = &tl.shots[tl.shot_index_at(tl.lines[1].start)];
        assert!(shot.shot.roll < 0.0);
    }

    #[test]
    fn captions_cover_the_spoken_lines_and_shout_in_capitals() {
        let tl = timeline(SHORT);
        let m = tl.measures();
        assert!(m.caption_coverage > 0.99, "{m:?}");
        assert!(tl.captions[0].text.chars().filter(|c| c.is_alphabetic()).all(char::is_uppercase));
        assert!(tl.caption_at(tl.duration + 1.0).is_none());
        assert_eq!(m.marks, ["flustered"]);
        assert_eq!(m.distinct_faces, 3);
        assert_eq!(m.changes, 2, "line 1 changes face and shot together; line 3 changes both again");
    }

    #[test]
    fn phrases_break_at_the_limit_and_after_punctuation() {
        assert_eq!(phrases("one two three four five six", 9), ["one two", "three", "four five", "six"]);
        assert_eq!(phrases("Okay, fine. Whatever you say", 24), ["Okay, fine.", "Whatever you say"]);
    }

    #[test]
    fn unknown_channels_are_refused_by_name() {
        let text = format!("{SHORT}channels = {{ notAChannel = 1.0 }}\n");
        let def = ShortDef::from_toml(&text).unwrap();
        let voice = voice_for(&def, |_| unreachable!()).unwrap();
        let err = Timeline::new(&def, &voice).unwrap_err().to_string();
        assert!(err.contains("line[2].channels.notAChannel"), "{err}");
    }
}
