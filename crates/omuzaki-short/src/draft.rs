//! `short.draft`'s rules (#2419): a plain script in, a directed short out. Each line gets a face from what it says
//! (its words and its punctuation), a shot that keeps the frame moving, and a caption style, so a first render already
//! changes face and shot as often as a hand-cut talking-head short does. A person or an agent then edits the file;
//! the drafter only removes the blank page.
//!
//! The rules, in order of precedence for a line:
//!
//! - **Asides** in parentheses or brackets: a small caption, the camera off to one side, a sly face.
//! - **Shouts** (a line in capitals, or ending in `!!` or `?!`): a big caption, a punch-in with shake, an angry or
//!   shocked face.
//! - **Cue words** (`love`, `wow`, `ugh`, `sorry`, `haha`, `um`, ...): their face, from [`CUES`].
//! - **Questions**: a puzzled face and a dutch tilt, alternating sides.
//! - **Trailing off** (`...`): gloom, the camera pushing in slowly.
//! - Anything else cycles through calm faces ([`CALM`]) and alternates a face shot with a closer one, with a bust shot
//!   every fourth line so the frame breathes.
//!
//! The same face or shot never runs twice in a row: a repeat moves to the next in its list. A line of
//! [`BEAT_WORDS`] words or more also gets a beat halfway (after its first comma or full stop when there is one): the
//! second sentence's cue face when it has one, and a cut, so a long line doesn't hold one frame.

use crate::def::{CaptionStyle, FaceCue, LineDef, ShortDef, ShotCue};

/// Cue words and the face each calls for. A line takes the face of its first cue word.
pub const CUES: [(&str, &str); 40] = [
    ("love", "hearts"),
    ("cute", "hearts"),
    ("adorable", "hearts"),
    ("wow", "stars"),
    ("amazing", "stars"),
    ("incredible", "stars"),
    ("perfect", "sparkle"),
    ("beautiful", "sparkle"),
    ("ugh", "cross"),
    ("seriously", "cross"),
    ("annoying", "cross"),
    ("hate", "angry"),
    ("stop", "angry"),
    ("sorry", "sad"),
    ("miss", "sad"),
    ("sad", "sad"),
    ("cry", "cry"),
    ("haha", "laugh"),
    ("lol", "laugh"),
    ("hehe", "smug"),
    ("um", "sweat"),
    ("uh", "sweat"),
    ("oops", "sweat"),
    ("wait", "surprised"),
    ("what", "surprised"),
    ("huh", "confused"),
    ("hmm", "confused"),
    ("actually", "smirk"),
    ("secret", "smirk"),
    ("obviously", "smug"),
    ("scary", "scared"),
    ("afraid", "scared"),
    ("tired", "sleepy"),
    ("sleepy", "sleepy"),
    ("ew", "disgusted"),
    ("gross", "disgusted"),
    ("blush", "embarrassed"),
    ("embarrassing", "embarrassed"),
    ("dizzy", "dizzy"),
    ("promise", "determined"),
];

/// The faces an undirected line cycles through.
pub const CALM: [&str; 6] = ["happy", "smug", "relaxed", "determined", "shy", "smirk"];

/// The shots an undirected line alternates between.
const CALM_SHOTS: [&str; 3] = ["face+drift", "close", "face+left+drift"];

/// The words a line needs before the drafter cuts inside it.
pub const BEAT_WORDS: usize = 7;

/// `text` with a beat inserted at its middle word (or after its first mid-line punctuation): a cut to `shot`, and the
/// second half's cue face when it has one. Text that already has beats is left alone.
fn with_beat(text: &str, shot: &str) -> String {
    let words: Vec<&str> = text.split_whitespace().collect();
    if words.len() < BEAT_WORDS || text.contains('{') {
        return text.to_owned();
    }
    let lo = words.len() / 3;
    let hi = words.len() - 2;
    let at = (lo..hi).find(|&i| words[i].ends_with([',', '.', ';', '!', '?', '…'])).map_or(words.len() / 2, |i| i + 1);
    let second = words[at..].join(" ");
    let face = cue_face(&second).map(|f| format!("{f} ")).unwrap_or_default();
    format!("{} {{{face}@{shot}}} {second}", words[..at].join(" "))
}

/// What kind of line it is, for the rules.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    Aside,
    Shout,
    Question,
    Trailing,
    Plain,
}

fn kind(text: &str) -> Kind {
    let t = text.trim();
    let letters: Vec<char> = t.chars().filter(|c| c.is_alphabetic()).collect();
    let capitals = letters.len() >= 3 && letters.iter().all(|c| c.is_uppercase());
    if (t.starts_with('(') && t.ends_with(')')) || (t.starts_with('[') && t.ends_with(']')) {
        Kind::Aside
    } else if capitals || t.ends_with("!!") || t.ends_with("?!") || t.ends_with("!?") {
        Kind::Shout
    } else if t.ends_with('?') {
        Kind::Question
    } else if t.ends_with("...") || t.ends_with('…') {
        Kind::Trailing
    } else {
        Kind::Plain
    }
}

/// The face of the first cue word in `text`.
fn cue_face(text: &str) -> Option<&'static str> {
    text.split(|c: char| !c.is_alphanumeric() && c != '\'')
        .map(str::to_lowercase)
        .find_map(|word| CUES.iter().find(|(cue, _)| *cue == word).map(|(_, face)| *face))
}

/// `pick`, or the next of `list` after it when it equals `last`.
fn not_again<'a>(pick: &'a str, last: Option<&str>, list: &[&'a str]) -> &'a str {
    if Some(pick) != last {
        return pick;
    }
    let at = list.iter().position(|p| *p == pick).unwrap_or(0);
    list[(at + 1) % list.len()]
}

/// The lines of `script`: one per non-empty line, a long one split at its sentence ends.
#[must_use]
pub fn script_lines(script: &str) -> Vec<String> {
    let mut out = Vec::new();
    for line in script.lines().map(str::trim).filter(|l| !l.is_empty()) {
        if line.split_whitespace().count() <= 16 {
            out.push(line.to_owned());
            continue;
        }
        let mut sentence = String::new();
        for word in line.split_whitespace() {
            if !sentence.is_empty() {
                sentence.push(' ');
            }
            sentence.push_str(word);
            if word.ends_with(['.', '!', '?']) && sentence.split_whitespace().count() >= 3 {
                out.push(std::mem::take(&mut sentence));
            }
        }
        if !sentence.is_empty() {
            out.push(sentence);
        }
    }
    out
}

/// Directs every line of `def` that has no face or no shot yet, by the rules above, leaving what a person set alone.
pub fn direct(def: &mut ShortDef) {
    let (mut last_face, mut last_shot): (Option<String>, Option<String>) = (None, None);
    let (mut calm, mut calm_shot, mut tilt) = (0usize, 0usize, false);
    for (i, line) in def.lines.iter_mut().enumerate() {
        let k = kind(&line.text);
        let cue = cue_face(&line.text);
        let (face, shot, caption) = match k {
            Kind::Aside => (cue.unwrap_or("smirk"), "close+right+drift", CaptionStyle::Aside),
            Kind::Shout => {
                let face = cue.unwrap_or(if line.text.contains('?') { "surprised" } else { "angry" });
                let caption = if line.text.split_whitespace().count() <= 2 { CaptionStyle::Big } else { CaptionStyle::Shout };
                (face, "close+punch+shake", caption)
            }
            Kind::Question => {
                tilt = !tilt;
                (cue.unwrap_or("confused"), if tilt { "face+tilt-left" } else { "face+tilt-right" }, CaptionStyle::Normal)
            }
            Kind::Trailing => (cue.unwrap_or("gloom"), "eyes+push", CaptionStyle::Normal),
            Kind::Plain => {
                let face = cue.unwrap_or_else(|| {
                    calm += 1;
                    CALM[(calm - 1) % CALM.len()]
                });
                let shot = if i % 4 == 3 {
                    "bust+push"
                } else {
                    calm_shot += 1;
                    CALM_SHOTS[(calm_shot - 1) % CALM_SHOTS.len()]
                };
                let caption = if cue.is_some() && line.text.split_whitespace().count() <= 2 {
                    CaptionStyle::Big
                } else {
                    CaptionStyle::Normal
                };
                (face, shot, caption)
            }
        };
        if line.face.is_none() {
            let face = not_again(face, last_face.as_deref(), &CALM);
            line.face = Some(FaceCue::One(face.to_owned()));
        }
        if line.camera.is_none() {
            let shot = not_again(shot, last_shot.as_deref(), &CALM_SHOTS);
            line.camera = Some(ShotCue::Words(shot.to_owned()));
        }
        if line.caption == CaptionStyle::Normal {
            line.caption = caption;
        }
        if matches!(k, Kind::Plain | Kind::Question) {
            let cut = if line.camera.as_ref().is_some_and(|c| matches!(c, ShotCue::Words(w) if w.starts_with("close"))) {
                "face+right"
            } else {
                "close"
            };
            line.text = with_beat(&line.text, cut);
        }
        if k == Kind::Aside && line.caption_text.is_none() {
            let inner = line.text.trim().trim_start_matches(['(', '[']).trim_end_matches([')', ']']).trim();
            line.caption_text = Some(inner.to_owned());
        }
        last_face = line.face.as_ref().and_then(|f| f.ids().first().cloned());
        last_shot = match &line.camera {
            Some(ShotCue::Words(w)) => Some(w.clone()),
            _ => None,
        };
    }
}

/// A directed short of `script` for `character`, at the defaults.
#[must_use]
pub fn draft(script: &str, character: omuzaki_char_api::CharId) -> ShortDef {
    let mut def = ShortDef::new(character);
    def.lines = script_lines(script).into_iter().map(LineDef::new).collect();
    direct(&mut def);
    def
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id() -> omuzaki_char_api::CharId {
        omuzaki_char_api::CharId::new("lumi").unwrap()
    }

    #[test]
    fn a_script_is_directed_line_by_line_without_repeats() {
        let script = "Okay so today I tried baking.\nIt was a DISASTER!!\n(don't tell anyone)\nWhy is flour everywhere?\n\
                      I love these cookies though.\nWow.\nI guess I'm just... not a baker...\nAnyway, see you tomorrow.";
        let def = draft(script, id());
        assert_eq!(def.lines.len(), 8);
        let faces: Vec<String> = def.lines.iter().map(|l| l.face.as_ref().unwrap().ids()[0].clone()).collect();
        assert_eq!(faces[1], "angry");
        assert_eq!(faces[2], "smirk");
        assert_eq!(faces[3], "confused");
        assert_eq!(faces[4], "hearts");
        assert_eq!(faces[5], "stars");
        assert_eq!(faces[6], "gloom");
        for pair in faces.windows(2) {
            assert_ne!(pair[0], pair[1], "{faces:?}");
        }
        assert_eq!(def.lines[1].caption, CaptionStyle::Shout);
        assert_eq!(def.lines[2].caption, CaptionStyle::Aside);
        assert_eq!(def.lines[2].caption_text.as_deref(), Some("don't tell anyone"));
        assert_eq!(def.lines[5].caption, CaptionStyle::Big);
        for line in &def.lines {
            line.camera.as_ref().unwrap().numbers().unwrap();
        }
        def.check().unwrap();
    }

    #[test]
    fn what_a_person_set_is_kept() {
        let mut def = ShortDef::new(id());
        let mut line = LineDef::new("Hello!!");
        line.face = Some(FaceCue::One("shy".into()));
        def.lines.push(line);
        direct(&mut def);
        assert_eq!(def.lines[0].face, Some(FaceCue::One("shy".into())));
        assert!(def.lines[0].camera.is_some());
    }

    #[test]
    fn a_long_line_cuts_halfway_and_takes_its_second_face() {
        let def = draft("Okay so I tried baking, and honestly I love how it turned out.", id());
        let line = &def.lines[0];
        assert!(line.text.contains("{hearts @"), "{}", line.text);
        assert_eq!(line.spoken(), "Okay so I tried baking, and honestly I love how it turned out.");
        let beats = line.beats().unwrap();
        assert_eq!(beats.len(), 1);
        assert!(beats[0].at > 0.3 && beats[0].at < 0.5, "{beats:?}");
    }

    #[test]
    fn a_long_line_splits_at_its_sentences() {
        let lines = script_lines(
            "This is the first sentence of a long paragraph. Then another one follows it right away! And a third one ends.",
        );
        assert_eq!(lines.len(), 3, "{lines:?}");
    }
}
