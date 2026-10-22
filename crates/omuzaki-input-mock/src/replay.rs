//! [`ReplaySource`]: recorded input as text, replayed as a [`MotionFeed`] in exactly the recorded order.
//!
//! One entry per line, in drained order (design §2: the live order is the order the hub drained its rings, and a replay
//! hands consumers exactly that sequence). Blank lines and `#` comments are skipped. Every line is
//! `<time> <backend> <kind> <fields>`, times in seconds, zones by name, never a key:
//!
//! ```text
//! # omuzaki recorded input 1
//! 0.008333333333333333 windows.raw_input source_up 1 keyboard -
//! 0.016666666666666666 windows.raw_input key 1 left_inner_lower down
//! 0.016666666666666666 windows.raw_input buttons 2 mouse 1 0
//! 0.025 windows.raw_input pointer 2 0.001 -0.002 0
//! 0.025 windows.raw_input pen 3 12 30 200 -5 10 range+contact
//! 0.03333333333333333 gamepad.gilrs pad_edges 0 face+dpad -
//! 0.03333333333333333 gamepad.gilrs pad 0 -127 127 0 5 255 0 face
//! 0.04 gamepad.gilrs source_gone 4
//! ```
//!
//! `pad` lines are levels, not events: draining past one updates what [`MotionFeed::pad`] answers. Numbers are written
//! in Rust's shortest round-trip form, so text written by [`ReplaySource::to_text`] reads back bit-identically.

use std::fmt::Write as _;
use std::str::SplitWhitespace;

use omuzaki_core::time::Seconds;
use omuzaki_input_api::event::{DeviceClass, MAX_PADS, MotionEvent, PadId, PadSet, PadState, PadZones, SourceTag};
use omuzaki_input_api::feed::{MotionFeed, MotionFrame};
use omuzaki_input_api::zones::KeyZone;

/// The first line [`ReplaySource::to_text`] writes.
pub const HEADER: &str = "# omuzaki recorded input 1";

/// A line of recorded input that could not be read.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("recorded input line {line}: {reason}")]
pub struct ReplayError {
    /// The line, from 1.
    pub line: usize,
    /// What is wrong with it.
    pub reason: String,
}

/// One recorded entry.
#[derive(Copy, Clone, Debug, PartialEq)]
pub enum Entry {
    /// A motion event, as drained.
    Event(MotionEvent),
    /// A pad's levels at that point.
    Pad(PadState),
}

impl Entry {
    fn at(&self) -> Seconds {
        match self {
            Self::Event(event) => event.at(),
            Self::Pad(levels) => levels.at,
        }
    }
}

/// Recorded input, built entry by entry ([`ReplaySource::push`]) or read from text, and replayed as a [`MotionFeed`].
#[derive(Clone, Debug, PartialEq)]
pub struct ReplaySource {
    backends: Vec<String>,
    entries: Vec<(u16, Entry)>,
    next: usize,
    pads: [Option<PadState>; MAX_PADS],
    /// The pad each connected source is, from its `source_up`, so its `source_gone` hides the pad.
    pad_of: [Option<PadId>; SourceTag::MAX],
}

impl Default for ReplaySource {
    fn default() -> Self {
        Self::new()
    }
}

impl ReplaySource {
    /// An empty recording.
    pub fn new() -> Self {
        Self {
            backends: Vec::new(),
            entries: Vec::new(),
            next: 0,
            pads: [None; MAX_PADS],
            pad_of: [None; SourceTag::MAX],
        }
    }

    /// Appends `entry`, drained from `backend`.
    pub fn push(&mut self, backend: &str, entry: Entry) {
        let index = self.backends.iter().position(|b| b == backend).unwrap_or_else(|| {
            self.backends.push(backend.to_owned());
            self.backends.len() - 1
        });
        self.entries.push((u16::try_from(index).unwrap_or(u16::MAX), entry));
    }

    /// The entries with their backends, in recorded order.
    pub fn entries(&self) -> impl Iterator<Item = (&str, Entry)> + '_ {
        self.entries.iter().map(|(b, e)| (self.backends.get(usize::from(*b)).map_or("", String::as_str), *e))
    }

    /// Starts the replay again from the first entry.
    pub fn rewind(&mut self) {
        self.next = 0;
        self.pads = [None; MAX_PADS];
        self.pad_of = [None; SourceTag::MAX];
    }

    /// Whether every entry has been drained.
    pub fn is_done(&self) -> bool {
        self.next >= self.entries.len()
    }

    /// Reads recorded input.
    ///
    /// # Errors
    /// A line that is not one of the forms in the module docs.
    pub fn from_text(text: &str) -> Result<Self, ReplayError> {
        let mut out = Self::new();
        for (n, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let fail = |reason: String| ReplayError { line: n + 1, reason };
            let mut words = line.split_whitespace();
            let at = Seconds(parse(&mut words, "time").map_err(fail)?);
            let backend = words.next().ok_or_else(|| fail("no backend".to_owned()))?;
            let entry = parse_entry(&mut words, at).map_err(fail)?;
            if let Some(extra) = words.next() {
                return Err(fail(format!("unexpected `{extra}`")));
            }
            out.push(backend, entry);
        }
        Ok(out)
    }

    /// Writes it as text that [`from_text`](Self::from_text) reads back identically.
    pub fn to_text(&self) -> String {
        let mut out = format!("{HEADER}\n");
        for (backend, entry) in self.entries() {
            let _ = write!(out, "{} {backend} ", entry.at().0);
            write_entry(&mut out, &entry);
            out.push('\n');
        }
        out
    }
}

fn parse<T: std::str::FromStr>(words: &mut SplitWhitespace<'_>, what: &str) -> Result<T, String> {
    let word = words.next().ok_or_else(|| format!("no {what}"))?;
    word.parse().map_err(|_| format!("`{word}` is not a {what}"))
}

fn source(words: &mut SplitWhitespace<'_>) -> Result<SourceTag, String> {
    let index: u8 = parse(words, "source")?;
    SourceTag::new(index).ok_or_else(|| format!("source {index} is out of range"))
}

fn pad_id(words: &mut SplitWhitespace<'_>) -> Result<PadId, String> {
    let index: u8 = parse(words, "pad")?;
    PadId::new(index).ok_or_else(|| format!("pad {index} is out of range"))
}

fn class(word: &str) -> Result<DeviceClass, String> {
    DEVICE_NAMES.iter().find(|(_, name)| *name == word).map(|(c, _)| *c).ok_or_else(|| format!("no device `{word}`"))
}

const DEVICE_NAMES: [(DeviceClass, &str); 6] = [
    (DeviceClass::Keyboard, "keyboard"),
    (DeviceClass::Mouse, "mouse"),
    (DeviceClass::Pen, "pen"),
    (DeviceClass::Gamepad, "gamepad"),
    (DeviceClass::Midi, "midi"),
    (DeviceClass::Osc, "osc"),
];

fn class_name(class: DeviceClass) -> &'static str {
    DEVICE_NAMES.iter().find(|(c, _)| *c == class).map_or("keyboard", |(_, name)| name)
}

fn zones(word: &str) -> Result<PadZones, String> {
    if word == "-" {
        return Ok(PadZones::NONE);
    }
    word.split('+').try_fold(PadZones::NONE, |set, name| {
        let zone = PadZones::NAMED.iter().find(|(_, n)| *n == name).ok_or_else(|| format!("no pad zone `{name}`"))?;
        Ok(set.or(zone.0))
    })
}

fn write_zones(out: &mut String, set: PadZones) {
    let mut names = PadZones::NAMED.iter().filter(|(zone, _)| set.contains(*zone)).map(|(_, name)| *name).peekable();
    if names.peek().is_none() {
        out.push('-');
    }
    for (i, name) in names.enumerate() {
        if i > 0 {
            out.push('+');
        }
        out.push_str(name);
    }
}

fn parse_entry(words: &mut SplitWhitespace<'_>, at: Seconds) -> Result<Entry, String> {
    let kind = words.next().ok_or("no kind")?;
    let event = match kind {
        "key" => {
            let src = source(words)?;
            let name = words.next().ok_or("no zone")?;
            let zone = KeyZone::from_name(name).ok_or_else(|| format!("no zone `{name}`"))?;
            let down = match words.next() {
                Some("down") => true,
                Some("up") => false,
                other => return Err(format!("`{}` is not down or up", other.unwrap_or_default())),
            };
            MotionEvent::key(src, zone, down, at)
        }
        "buttons" => {
            let src = source(words)?;
            let device = class(words.next().ok_or("no device")?)?;
            MotionEvent::buttons(src, device, parse(words, "button mask")?, parse(words, "button mask")?, at)
        }
        "pointer" => {
            let src = source(words)?;
            MotionEvent::pointer(src, parse(words, "dx")?, parse(words, "dy")?, parse(words, "wheel")?, at)
        }
        "pen" => {
            let src = source(words)?;
            let (u, v, pressure) = (parse(words, "u")?, parse(words, "v")?, parse(words, "pressure")?);
            let tilt = [parse(words, "tilt")?, parse(words, "tilt")?];
            let flags = words.next().ok_or("no pen flags")?;
            let (in_range, contact) = match flags {
                "-" => (false, false),
                "range" => (true, false),
                "contact" => (false, true),
                "range+contact" => (true, true),
                other => return Err(format!("`{other}` is not pen flags")),
            };
            MotionEvent::pen(src, u, v, pressure, tilt, in_range, contact, at)
        }
        "pad_edges" => {
            let pad = pad_id(words)?;
            let pressed = zones(words.next().ok_or("no pressed zones")?)?;
            MotionEvent::pad_edges(pad, pressed, zones(words.next().ok_or("no released zones")?)?, at)
        }
        "source_up" => {
            let src = source(words)?;
            let device = class(words.next().ok_or("no device")?)?;
            let pad = match words.next() {
                Some("-") => None,
                Some(word) => {
                    let index: u8 = word.parse().map_err(|_| format!("`{word}` is not a pad"))?;
                    Some(PadId::new(index).ok_or_else(|| format!("pad {index} is out of range"))?)
                }
                None => return Err("no pad".to_owned()),
            };
            MotionEvent::source_up(src, device, pad, at)
        }
        "source_gone" => MotionEvent::source_gone(source(words)?, at),
        "pad" => {
            let pad = pad_id(words)?;
            let sticks =
                [[parse(words, "stick")?, parse(words, "stick")?], [parse(words, "stick")?, parse(words, "stick")?]];
            let triggers = [parse(words, "trigger")?, parse(words, "trigger")?];
            let zones = zones(words.next().ok_or("no zones")?)?;
            return Ok(Entry::Pad(PadState { pad, sticks, triggers, zones, at }));
        }
        other => return Err(format!("no entry kind `{other}`")),
    };
    Ok(Entry::Event(event))
}

fn write_entry(out: &mut String, entry: &Entry) {
    let _ = match *entry {
        Entry::Event(MotionEvent::Key { src, zone, down, .. }) => {
            write!(out, "key {} {} {}", src.index(), zone.name(), if down { "down" } else { "up" })
        }
        Entry::Event(MotionEvent::Buttons { src, device, pressed, released, .. }) => {
            write!(out, "buttons {} {} {pressed} {released}", src.index(), class_name(device))
        }
        Entry::Event(MotionEvent::Pointer { src, dx, dy, wheel, .. }) => {
            write!(out, "pointer {} {dx} {dy} {wheel}", src.index())
        }
        Entry::Event(MotionEvent::Pen { src, u, v, pressure, tilt, in_range, contact, .. }) => {
            let flags = match (in_range, contact) {
                (false, false) => "-",
                (true, false) => "range",
                (false, true) => "contact",
                (true, true) => "range+contact",
            };
            write!(out, "pen {} {u} {v} {pressure} {} {} {flags}", src.index(), tilt[0], tilt[1])
        }
        Entry::Event(MotionEvent::PadEdges { pad, pressed, released, .. }) => {
            let _ = write!(out, "pad_edges {} ", pad.index());
            write_zones(out, pressed);
            out.push(' ');
            write_zones(out, released);
            Ok(())
        }
        Entry::Event(MotionEvent::SourceUp { src, class, pad, .. }) => {
            let pad = pad.map_or_else(|| "-".to_owned(), |p| p.index().to_string());
            write!(out, "source_up {} {} {pad}", src.index(), class_name(class))
        }
        Entry::Event(MotionEvent::SourceGone { src, .. }) => write!(out, "source_gone {}", src.index()),
        Entry::Pad(PadState { pad, sticks, triggers, zones, .. }) => {
            let [[lx, ly], [rx, ry]] = sticks;
            let _ = write!(out, "pad {} {lx} {ly} {rx} {ry} {} {} ", pad.index(), triggers[0], triggers[1]);
            write_zones(out, zones);
            Ok(())
        }
    };
}

impl ReplaySource {
    fn track_pads(&mut self, event: &MotionEvent) {
        match *event {
            MotionEvent::SourceUp { src, pad, .. } => {
                if let Some(slot) = self.pad_of.get_mut(usize::from(src.index())) {
                    *slot = pad;
                }
            }
            MotionEvent::SourceGone { src, .. } => {
                let pad = self.pad_of.get_mut(usize::from(src.index())).and_then(Option::take);
                if let Some(slot) = pad.and_then(|pad| self.pads.get_mut(usize::from(pad.index()))) {
                    *slot = None;
                }
            }
            _ => {}
        }
    }
}

impl MotionFeed for ReplaySource {
    fn drain(&mut self, up_to: Seconds, out: &mut MotionFrame) {
        out.clear();
        while let Some((_, entry)) = self.entries.get(self.next) {
            if entry.at().0 > up_to.0 {
                break;
            }
            match *entry {
                Entry::Event(event) => {
                    if out.push(event).is_err() {
                        break;
                    }
                    self.track_pads(&event);
                }
                Entry::Pad(levels) => {
                    if let Some(slot) = self.pads.get_mut(usize::from(levels.pad.index())) {
                        *slot = Some(levels);
                    }
                }
            }
            self.next += 1;
        }
    }

    fn pad(&self, pad: PadId) -> Option<PadState> {
        self.pads.get(usize::from(pad.index())).copied().flatten()
    }

    fn pads(&self, out: &mut PadSet) {
        out.clear();
        for levels in self.pads.iter().flatten() {
            out.push(levels.pad);
        }
    }
}
