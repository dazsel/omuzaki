//! Design §3 rule 1: no public type of the contract holds a key code, a chord or a usage, and `ChordLabel`, the one
#![allow(clippy::unwrap_used)] // a panic is a test failure
//! stated exception, cannot be serialised, displayed or debug-printed as its text.

use std::collections::BTreeSet;
use std::marker::PhantomData;
use std::path::Path;

use omuzaki_input_api::binding::{BindingAction, BindingInfo, ChordLabel, Edge, Intent, IntentEvent};
use omuzaki_input_api::capture::{
    BackendReport, BackendState, CaptureCaps, CaptureReport, CaptureState, Detail, PauseWhy, PermissionHow, Support,
};
use omuzaki_input_api::event::{
    DeviceClass, DeviceMask, MotionEvent, PadId, PadState, PadZones, SourceMask, SourceTag,
};
use omuzaki_input_api::zones::{Band, Finger, KeyZone, Side, ZoneInfo};
use serde_json::Value;

/// Words a field, a variant or a type name of the contract may not contain.
const FORBIDDEN: &[&str] = &[
    "keycode",
    "key_code",
    "scancode",
    "scan_code",
    "usage",
    "chord",
    "vk",
    "hid",
    "char",
    "text",
    "cursor",
    "screen",
];

/// Every name in a schema: property names, enum values, `const` values and definition names.
fn names(value: &Value, out: &mut BTreeSet<String>) {
    match value {
        Value::Object(map) => {
            for (key, inner) in map {
                if let ("properties" | "$defs", Value::Object(fields)) = (key.as_str(), inner) {
                    out.extend(fields.keys().cloned());
                }
                if let ("enum", Value::Array(values)) = (key.as_str(), inner) {
                    out.extend(values.iter().filter_map(Value::as_str).map(str::to_owned));
                }
                if let ("const", Value::String(value)) = (key.as_str(), inner) {
                    out.insert(value.clone());
                }
                names(inner, out);
            }
        }
        Value::Array(items) => items.iter().for_each(|item| names(item, out)),
        _ => {}
    }
}

macro_rules! schemas {
    ($($ty:ty),* $(,)?) => {{
        let mut out = BTreeSet::new();
        $(
            out.insert(stringify!($ty).to_owned());
            names(&serde_json::to_value(schemars::schema_for!($ty)).unwrap(), &mut out);
        )*
        out
    }};
}

#[test]
fn no_public_type_holds_a_key_code_a_chord_or_a_usage() {
    let all = schemas!(
        KeyZone,
        ZoneInfo,
        Side,
        Finger,
        Band,
        DeviceClass,
        DeviceMask,
        SourceTag,
        SourceMask,
        PadId,
        PadZones,
        PadState,
        MotionEvent,
        BindingAction,
        Intent,
        Edge,
        IntentEvent,
        BindingInfo,
        CaptureState,
        BackendState,
        BackendReport,
        CaptureCaps,
        CaptureReport,
        Detail,
        PauseWhy,
        PermissionHow,
        Support,
    );
    assert!(all.len() > 60, "the walk found the fields: {all:?}");
    for name in &all {
        let lower = name.to_ascii_lowercase();
        assert!(
            !FORBIDDEN.iter().any(|word| lower.split(|c: char| !c.is_ascii_alphanumeric()).any(|part| part == *word)
                || lower.contains("keycode")
                || lower.contains("scancode")
                || lower.contains("chord")
                || lower.contains("usage")),
            "`{name}` names a key identity"
        );
    }
}

#[test]
fn the_crate_source_names_no_hid_usage_or_key_code_constant() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    for entry in std::fs::read_dir(src).unwrap() {
        let path = entry.unwrap().path();
        let text = std::fs::read_to_string(&path).unwrap();
        for (n, line) in text.lines().enumerate() {
            let code = line.split("//").next().unwrap_or_default();
            for word in ["HidUsage", "hid_usage", "usage", "VK_", "KEY_", "kVK", "scancode", "scan_code", "keycode"] {
                assert!(!code.contains(word), "{}:{} names `{word}`: {line}", path.display(), n + 1);
            }
        }
    }
}

/// `IS` is true only when `T: Serialize`: the inherent constant wins when its bound holds, else the trait's default.
struct Serializes<T>(PhantomData<T>);
trait No {
    const IS: bool = false;
}
impl<T> No for Serializes<T> {}
impl<T: serde::Serialize> Serializes<T> {
    const IS: bool = true;
}

struct Deserializes<T>(PhantomData<T>);
impl<T> No for Deserializes<T> {}
impl<T: serde::de::DeserializeOwned> Deserializes<T> {
    const IS: bool = true;
}

struct Displays<T>(PhantomData<T>);
impl<T> No for Displays<T> {}
impl<T: std::fmt::Display> Displays<T> {
    const IS: bool = true;
}

// Checked when the test compiles: a `Serialize`, `Deserialize` or `Display` for `ChordLabel` fails the build.
const _: () = assert!(!Serializes::<ChordLabel>::IS && !Deserializes::<ChordLabel>::IS && !Displays::<ChordLabel>::IS);
// The check itself works: these types do have them.
const _: () = assert!(Serializes::<KeyZone>::IS && Deserializes::<KeyZone>::IS && Displays::<String>::IS);

#[test]
fn a_chord_label_prints_only_a_placeholder_and_renders_its_text_on_request() {
    let label = ChordLabel::new("Ctrl+Shift+F9");
    assert_eq!(format!("{label:?}"), "<chord>");
    assert_eq!(format!("{:?}", Some(&label)), "Some(<chord>)");
    let mut shown = String::new();
    label.render(&mut |text| shown.push_str(text));
    assert_eq!(shown, "Ctrl+Shift+F9");
}
