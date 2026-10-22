//! What the rings carry, as words: a motion entry ([`MotionEvent`] plus its stamps and detail generation) and a
//! binding entry (a resolved hit's generation, slot, binding, source, edge and time, plus for an intent the intent).
//! No entry holds a key code: the capture thread has already turned the key into a zone or a binding.

// An entry packs small fields into `u64` words and reads them back: the narrowing casts are the unpacking.
#![allow(clippy::cast_possible_truncation)]

use omuzaki_core::time::Seconds;
use omuzaki_input_api::binding::{BindingId, Edge, Intent};
use omuzaki_input_api::event::{DeviceClass, MotionEvent, PadId, PadZones, SourceTag};
use omuzaki_input_api::zones::KeyZone;

/// Words in a motion entry.
pub(crate) const MOTION_WORDS: usize = 5;
/// Words in a binding or intent entry.
pub(crate) const BINDING_WORDS: usize = 4;

/// A motion entry.
pub(crate) type MotionWords = [u64; MOTION_WORDS];
/// A binding or intent entry.
pub(crate) type BindingWords = [u64; BINDING_WORDS];

const KEY: u8 = 0;
const BUTTONS: u8 = 1;
const POINTER: u8 = 2;
const PEN: u8 = 3;
const PAD_EDGES: u8 = 4;
const SOURCE_UP: u8 = 5;
const SOURCE_GONE: u8 = 6;

fn class_index(class: DeviceClass) -> u8 {
    DeviceClass::ALL.iter().position(|c| *c == class).and_then(|i| u8::try_from(i).ok()).unwrap_or(0)
}

fn class_of(index: u64) -> DeviceClass {
    usize::try_from(index).ok().and_then(|i| DeviceClass::ALL.get(i)).copied().unwrap_or(DeviceClass::Keyboard)
}

fn byte(word: u64, at: u32) -> u8 {
    (word >> at) as u8
}

/// Packs a motion event with its stamps and detail generation.
pub(crate) fn motion(stamp: u64, detail: u32, event: &MotionEvent) -> MotionWords {
    let head = |kind: u8, src: u8, b2: u8, b3: u8| {
        u64::from(detail) | u64::from(kind) << 32 | u64::from(src) << 40 | u64::from(b2) << 48 | u64::from(b3) << 56
    };
    let at = event.at().0.to_bits();
    match *event {
        MotionEvent::Key { src, zone, down, .. } => [stamp, head(KEY, src.index(), zone.index(), u8::from(down)), at, 0, 0],
        MotionEvent::Buttons { src, device, pressed, released, .. } => {
            let payload = u64::from(pressed) | u64::from(released) << 8;
            [stamp, head(BUTTONS, src.index(), class_index(device), 0), at, payload, 0]
        }
        MotionEvent::Pointer { src, dx, dy, wheel, .. } => {
            let payload = u64::from(dx.to_bits()) | u64::from(dy.to_bits()) << 32;
            [stamp, head(POINTER, src.index(), 0, 0), at, payload, u64::from(wheel.to_bits())]
        }
        MotionEvent::Pen { src, u, v, pressure, tilt, in_range, contact, .. } => {
            let payload = u64::from(u)
                | u64::from(v) << 8
                | u64::from(pressure) << 16
                | u64::from(tilt[0].cast_unsigned()) << 24
                | u64::from(tilt[1].cast_unsigned()) << 32
                | u64::from(in_range) << 40
                | u64::from(contact) << 48;
            [stamp, head(PEN, src.index(), 0, 0), at, payload, 0]
        }
        MotionEvent::PadEdges { pad, pressed, released, .. } => {
            let payload = u64::from(pressed.bits()) | u64::from(released.bits()) << 16;
            [stamp, head(PAD_EDGES, 0, pad.index(), 0), at, payload, 0]
        }
        MotionEvent::SourceUp { src, class, pad, .. } => {
            let pad = pad.map_or(0, |p| p.index() + 1);
            [stamp, head(SOURCE_UP, src.index(), class_index(class), pad), at, 0, 0]
        }
        MotionEvent::SourceGone { src, .. } => [stamp, head(SOURCE_GONE, src.index(), 0, 0), at, 0, 0],
    }
}

/// The detail generation of a motion entry.
pub(crate) const fn motion_detail(words: &MotionWords) -> u32 {
    words[1] as u32
}

/// Whether a motion entry is a key edge (the entries a `detail` change drops).
pub(crate) fn motion_is_key(words: &MotionWords) -> bool {
    byte(words[1], 32) == KEY
}

/// Unpacks a motion entry's event.
pub(crate) fn motion_event(words: &MotionWords) -> Option<MotionEvent> {
    let [_, head, at, payload, extra] = *words;
    let (kind, src, b2, b3) = (byte(head, 32), byte(head, 40), byte(head, 48), byte(head, 56));
    let at = Seconds(f64::from_bits(at));
    let src = SourceTag::new(src)?;
    Some(match kind {
        KEY => MotionEvent::key(src, KeyZone::new(b2)?, b3 != 0, at),
        BUTTONS => MotionEvent::buttons(src, class_of(u64::from(b2)), byte(payload, 0), byte(payload, 8), at),
        POINTER => MotionEvent::pointer(
            src,
            f32::from_bits(payload as u32),
            f32::from_bits((payload >> 32) as u32),
            f32::from_bits(extra as u32),
            at,
        ),
        PEN => MotionEvent::pen(
            src,
            byte(payload, 0),
            byte(payload, 8),
            byte(payload, 16),
            [byte(payload, 24).cast_signed(), byte(payload, 32).cast_signed()],
            byte(payload, 40) != 0,
            byte(payload, 48) != 0,
            at,
        ),
        PAD_EDGES => MotionEvent::pad_edges(
            PadId::new(b2)?,
            PadZones::from_bits(payload as u16),
            PadZones::from_bits((payload >> 16) as u16),
            at,
        ),
        SOURCE_UP => MotionEvent::source_up(src, class_of(u64::from(b2)), b3.checked_sub(1).and_then(PadId::new), at),
        SOURCE_GONE => MotionEvent::source_gone(src, at),
        _ => return None,
    })
}

/// A binding hit as the ring carries it.
#[derive(Copy, Clone, Debug, PartialEq)]
pub(crate) struct HitEntry {
    /// The generation of the table it was matched against.
    pub(crate) generation: u32,
    /// Its slot in that table.
    pub(crate) slot: u8,
    /// The device that sent it.
    pub(crate) src: SourceTag,
    /// The binding.
    pub(crate) id: BindingId,
    /// Press, release or repeat.
    pub(crate) edge: Edge,
    /// When.
    pub(crate) at: Seconds,
    /// For an intent binding, the intent its press started.
    pub(crate) intent: Option<Intent>,
}

fn edge_index(edge: Edge) -> u8 {
    match edge {
        Edge::Press => 0,
        Edge::Release => 1,
        Edge::Repeat => 2,
    }
}

fn edge_of(index: u8) -> Edge {
    match index {
        0 => Edge::Press,
        1 => Edge::Release,
        _ => Edge::Repeat,
    }
}

fn intent_word(intent: Option<Intent>) -> u64 {
    let (tag, a, b) = match intent {
        None => (0u8, 0u8, 0u8),
        Some(Intent::Move { x, y, .. }) => (1, x.cast_unsigned(), y.cast_unsigned()),
        Some(Intent::Run) => (2, 0, 0),
        Some(Intent::Jump) => (3, 0, 0),
        Some(Intent::Sit) => (4, 0, 0),
        Some(Intent::Emote(n)) => (5, n, 0),
    };
    u64::from(tag) | u64::from(a) << 8 | u64::from(b) << 16
}

fn intent_of(word: u64) -> Option<Intent> {
    let (a, b) = (byte(word, 8), byte(word, 16));
    match byte(word, 0) {
        1 => Some(Intent::moving(a.cast_signed(), b.cast_signed())),
        2 => Some(Intent::Run),
        3 => Some(Intent::Jump),
        4 => Some(Intent::Sit),
        5 => Some(Intent::Emote(a)),
        _ => None,
    }
}

/// Packs a hit with its stamps.
pub(crate) fn hit(stamp: u64, hit: &HitEntry) -> BindingWords {
    let head = u64::from(hit.generation)
        | u64::from(hit.slot) << 32
        | u64::from(hit.src.index()) << 40
        | u64::from(edge_index(hit.edge)) << 48;
    [stamp, head, hit.at.0.to_bits(), u64::from(hit.id.0) | intent_word(hit.intent) << 32]
}

/// Unpacks a hit.
pub(crate) fn hit_of(words: &BindingWords) -> Option<HitEntry> {
    let [_, head, at, tail] = *words;
    Some(HitEntry {
        generation: head as u32,
        slot: byte(head, 32),
        src: SourceTag::new(byte(head, 40))?,
        edge: edge_of(byte(head, 48)),
        at: Seconds(f64::from_bits(at)),
        id: BindingId(tail as u32),
        intent: intent_of(tail >> 32),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_motion_event_and_hit_survives_its_words() {
        let src = SourceTag::new(63).unwrap_or_else(|| unreachable!());
        let pad = PadId::new(3).unwrap_or_else(|| unreachable!());
        let at = Seconds(12.345_678_9);
        for event in [
            MotionEvent::key(src, KeyZone::RIGHT_ONLY, true, at),
            MotionEvent::buttons(src, DeviceClass::Pen, 0b101, 0b010, at),
            MotionEvent::pointer(src, -0.001_25, 3.5e-4, -2.0, at),
            MotionEvent::pen(src, 31, 7, 255, [-40, 39], true, false, at),
            MotionEvent::pad_edges(pad, PadZones::ALL, PadZones::CENTRE, at),
            MotionEvent::source_up(src, DeviceClass::Gamepad, Some(pad), at),
            MotionEvent::source_up(src, DeviceClass::Mouse, None, at),
            MotionEvent::source_gone(src, at),
        ] {
            let words = motion(7, 9, &event);
            assert_eq!(motion_event(&words), Some(event));
            assert_eq!(motion_detail(&words), 9);
        }
        for intent in [None, Some(Intent::moving(-128, 127)), Some(Intent::Emote(200)), Some(Intent::Sit)] {
            for edge in [Edge::Press, Edge::Release, Edge::Repeat] {
                let entry = HitEntry { generation: 4_000_000_000, slot: 63, src, id: BindingId(u32::MAX), edge, at, intent };
                assert_eq!(hit_of(&hit(1, &entry)), Some(entry));
            }
        }
    }
}
