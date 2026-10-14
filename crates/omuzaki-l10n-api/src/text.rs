//! `Text`: a message id and typed arguments (ADR 0023 decision 3; design `docs/design/l10n-ops.md` section 4).
//!
//! Errors, findings and reasons carry a [`Text`] instead of a sentence. Every surface renders it in the reader's
//! language; records store it as plain data, so History shows old entries in the current language.

use std::borrow::Cow;
use std::fmt;

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::de::{MapAccess, Visitor};
use serde::ser::SerializeMap;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A localisable phrase: the id of a message and the values for its placeables.
///
/// Built by the functions `messages!` generates; read by the catalog, the UI, the CLI and the checks. In JSON it is
/// `{"id": "char-part-not-found", "args": {"part": {"id": "hair-b"}}}`, with `args` left out when empty.
#[derive(Debug, Clone, PartialEq)]
pub struct Text {
    /// The message id (`char-part-not-found`). Static for declared messages, owned for ids read from a record.
    pub id: Cow<'static, str>,
    /// The arguments in declaration order, by placeable name.
    pub args: Vec<(Cow<'static, str>, Arg)>,
}

impl Text {
    /// A message without arguments.
    pub fn new(id: impl Into<Cow<'static, str>>) -> Self {
        Self { id: id.into(), args: Vec::new() }
    }

    /// Adds one argument; `messages!` and tests use it to build a `Text` by hand. An argument of the same name is
    /// replaced (last write wins), so a `Text` built here never holds duplicates.
    #[must_use]
    pub fn with(mut self, name: impl Into<Cow<'static, str>>, arg: Arg) -> Self {
        let name = name.into();
        match self.args.iter_mut().find(|(n, _)| *n == name) {
            // One value per placeable: a repeat would serialise as a duplicate key, which loading refuses.
            Some((_, slot)) => *slot = arg,
            None => self.args.push((name, arg)),
        }
        self
    }

    /// The argument called `name`, if there is one.
    pub fn arg(&self, name: &str) -> Option<&Arg> {
        self.args.iter().find(|(n, _)| n == name).map(|(_, a)| a)
    }
}

impl fmt::Display for Text {
    /// The last-resort form of the fallback chain (design 9.1): `⟦id: name=value, …⟧`. Real rendering is the
    /// catalog's job; this exists so a `Text` is never blank in a log.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "⟦{}", self.id)?;
        for (i, (name, arg)) in self.args.iter().enumerate() {
            write!(f, "{} {name}={arg}", if i == 0 { ":" } else { "," })?;
        }
        f.write_str("⟧")
    }
}

/// One argument of a [`Text`]. Externally tagged and in snake case in JSON: `{"int": 3}`, `{"id": "hair-b"}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Arg {
    /// An op id, part id, scope, path, file name or hex colour: shown as it is, never translated.
    Id(String),
    /// A count. Drives plural selectors; the locale picks digits and grouping.
    Int(i64),
    /// A measured value; the locale picks the decimal separator. Must be finite: serialising NaN or infinity is an
    /// error (JSON has no such number), and [`Arg::num`] refuses them.
    Num(#[serde(serialize_with = "finite")] f64),
    /// A value with a unit (12.5 cm, 30°, 40 %), formatted by the locale. The unit stays an id in data.
    Qty {
        /// The number, in the unit given. Must be finite, as for [`Arg::Num`].
        #[serde(serialize_with = "finite")]
        value: f64,
        /// What it is measured in.
        unit: Unit,
    },
    /// A nested localisable phrase, such as a part-kind name.
    Text(Box<Text>),
    /// A list, joined with the locale's list pattern ("a, b and c"), never by the caller.
    List(Vec<Arg>),
    /// English that cannot be translated (an OS error, serde's reason): shown as is, in the error's details.
    Verbatim(String),
}

/// A non-finite number was given where an argument needs a finite one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("argument numbers must be finite (got NaN or infinity)")]
pub struct NotFinite;

impl Arg {
    /// A measured number.
    ///
    /// # Errors
    /// [`NotFinite`] for NaN and infinity, which JSON cannot carry.
    pub fn num(value: f64) -> Result<Self, NotFinite> {
        if value.is_finite() { Ok(Self::Num(value)) } else { Err(NotFinite) }
    }

    /// A number with a unit.
    ///
    /// # Errors
    /// [`NotFinite`] for NaN and infinity.
    pub fn qty(value: f64, unit: Unit) -> Result<Self, NotFinite> {
        if value.is_finite() { Ok(Self::Qty { value, unit }) } else { Err(NotFinite) }
    }

    /// What `messages!` uses for `Num` parameters, which cannot fail: a non-finite value becomes `Verbatim("NaN")`
    /// or `Verbatim("inf")`, so the text is still shown and always stores. A debug build panics instead: a caller
    /// passing a non-finite value is a bug to find in development.
    pub fn num_or_verbatim(value: f64) -> Self {
        Self::num(value).unwrap_or_else(|_| {
            debug_assert!(false, "a message argument must be finite, got {value}");
            Self::Verbatim(value.to_string())
        })
    }

    /// The `Qty` counterpart of [`num_or_verbatim`](Self::num_or_verbatim).
    pub fn qty_or_verbatim(value: f64, unit: Unit) -> Self {
        Self::qty(value, unit).unwrap_or_else(|_| {
            debug_assert!(false, "a message argument must be finite, got {value}");
            Self::Verbatim(format!("{value} {}", unit.symbol()))
        })
    }
}

/// Refuses to write a non-finite number, which `serde_json` would turn into `null` and fail to load again.
#[allow(clippy::trivially_copy_pass_by_ref)] // the signature `serialize_with` requires
fn finite<S: Serializer>(value: &f64, s: S) -> Result<S::Ok, S::Error> {
    if value.is_finite() { s.serialize_f64(*value) } else { Err(serde::ser::Error::custom(NotFinite)) }
}

impl fmt::Display for Arg {
    /// A plain form for [`Text`]'s last-resort display.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Id(s) | Self::Verbatim(s) => f.write_str(s),
            Self::Int(n) => write!(f, "{n}"),
            Self::Num(n) => write!(f, "{n}"),
            Self::Qty { value, unit } => write!(f, "{value} {}", unit.symbol()),
            Self::Text(t) => write!(f, "{t}"),
            Self::List(items) => {
                f.write_str("[")?;
                for (i, item) in items.iter().enumerate() {
                    write!(f, "{}{item}", if i == 0 { "" } else { ", " })?;
                }
                f.write_str("]")
            }
        }
    }
}

/// The units a [`Arg::Qty`] can carry: the set the locale formatters know. Snake case in JSON, like the units
/// in op data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Unit {
    /// Metres.
    M,
    /// Centimetres.
    Cm,
    /// Millimetres.
    Mm,
    /// Degrees of angle.
    Deg,
    /// Milliseconds.
    Ms,
    /// Seconds.
    S,
    /// Percent.
    Percent,
    /// Pixels.
    Px,
    /// Hertz.
    Hz,
    /// Decibels.
    Db,
}

impl Unit {
    /// The unit's ASCII symbol, for the fallback display and logs.
    pub const fn symbol(self) -> &'static str {
        match self {
            Self::M => "m",
            Self::Cm => "cm",
            Self::Mm => "mm",
            Self::Deg => "deg",
            Self::Ms => "ms",
            Self::S => "s",
            Self::Percent => "%",
            Self::Px => "px",
            Self::Hz => "Hz",
            Self::Db => "dB",
        }
    }
}

/// The wire form of [`Text`]: `args` is a JSON object, kept in order.
struct ArgMap<'a>(&'a [(Cow<'static, str>, Arg)]);

impl Serialize for ArgMap<'_> {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut map = s.serialize_map(Some(self.0.len()))?;
        for (i, (name, arg)) in self.0.iter().enumerate() {
            // `args` is public, so a duplicate can be built by hand; writing it would store a record that cannot load.
            if self.0[..i].iter().any(|(earlier, _)| earlier == name) {
                return Err(serde::ser::Error::custom(format!("argument `{name}` given twice")));
            }
            map.serialize_entry(name, arg)?;
        }
        map.end()
    }
}

impl Serialize for Text {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut map = s.serialize_map(Some(1 + usize::from(!self.args.is_empty())))?;
        map.serialize_entry("id", &self.id)?;
        if !self.args.is_empty() {
            map.serialize_entry("args", &ArgMap(&self.args))?;
        }
        map.end()
    }
}

/// Reads the `args` object into an ordered list, refusing a repeated name (two values for one placeable).
#[derive(Default)]
struct OwnedArgs(Vec<(Cow<'static, str>, Arg)>);

impl<'de> Deserialize<'de> for OwnedArgs {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = OwnedArgs;

            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("an object of named arguments")
            }

            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<OwnedArgs, A::Error> {
                let mut args: Vec<(Cow<'static, str>, Arg)> = Vec::new();
                while let Some((name, arg)) = map.next_entry::<String, Arg>()? {
                    if args.iter().any(|(n, _)| *n == name) {
                        return Err(serde::de::Error::custom(format!("argument `{name}` given twice")));
                    }
                    args.push((Cow::Owned(name), arg));
                }
                Ok(OwnedArgs(args))
            }
        }
        d.deserialize_map(V)
    }
}

impl<'de> Deserialize<'de> for Text {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            id: String,
            #[serde(default)]
            args: OwnedArgs,
        }
        let Wire { id, args } = Wire::deserialize(d)?;
        Ok(Self { id: Cow::Owned(id), args: args.0 })
    }
}

impl JsonSchema for Text {
    fn schema_name() -> Cow<'static, str> {
        Cow::Borrowed("Text")
    }

    fn json_schema(generator: &mut SchemaGenerator) -> Schema {
        let arg = generator.subschema_for::<Arg>();
        json_schema!({
            "description": "A localisable phrase: a message id and its named arguments.",
            "type": "object",
            "properties": {
                "id": { "type": "string" },
                "args": { "type": "object", "additionalProperties": arg }
            },
            "required": ["id"],
            "additionalProperties": false
        })
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::Message;

    fn sample() -> Text {
        Text::new("char-part-not-found")
            .with("part", Arg::Id("hair-b".into()))
            .with("count", Arg::Int(2))
            .with("len", Arg::Qty { value: 12.5, unit: Unit::Cm })
            .with("kind", Arg::Text(Box::new(Text::new("kind-twin_tails"))))
            .with("all", Arg::List(vec![Arg::Num(0.5), Arg::Verbatim("EACCES".into())]))
    }

    #[test]
    fn json_is_id_and_named_args_with_snake_case_tags() {
        assert_eq!(
            serde_json::to_value(sample()).unwrap(),
            json!({"id": "char-part-not-found", "args": {
                "part": {"id": "hair-b"},
                "count": {"int": 2},
                "len": {"qty": {"value": 12.5, "unit": "cm"}},
                "kind": {"text": {"id": "kind-twin_tails"}},
                "all": {"list": [{"num": 0.5}, {"verbatim": "EACCES"}]},
            }})
        );
        assert_eq!(serde_json::to_value(Text::new("a-b")).unwrap(), json!({"id": "a-b"}));
    }

    #[test]
    fn repeating_an_argument_name_replaces_it_so_history_still_loads() {
        let text = Text::new("a-b").with("count", Arg::Int(1)).with("other", Arg::Int(9)).with("count", Arg::Int(2));
        assert_eq!(text.args.len(), 2);
        assert_eq!(text.arg("count"), Some(&Arg::Int(2)));
        let back: Text = serde_json::from_str(&serde_json::to_string(&text).unwrap()).unwrap();
        assert_eq!(back, text);
    }

    #[test]
    fn non_finite_numbers_are_refused_and_never_serialise_as_null() {
        assert_eq!(Arg::num(f64::INFINITY), Err(NotFinite));
        assert_eq!(Arg::num(f64::NAN), Err(NotFinite));
        assert_eq!(Arg::qty(f64::NEG_INFINITY, Unit::Cm), Err(NotFinite));
        assert_eq!(Arg::num(0.5), Ok(Arg::Num(0.5)));
        assert!(serde_json::to_string(&Arg::Num(f64::INFINITY)).is_err());
        assert!(serde_json::to_string(&Arg::Qty { value: f64::NAN, unit: Unit::S }).is_err());
        let nested = Text::new("a-b")
            .with("x", Arg::List(vec![Arg::Text(Box::new(Text::new("c-d").with("y", Arg::Num(f64::INFINITY))))]));
        assert!(serde_json::to_string(&nested).is_err());
        assert!(serde_json::from_str::<Arg>(r#"{"num":null}"#).is_err());
    }

    #[test]
    fn duplicate_names_built_through_the_public_field_fail_to_serialise_naming_the_arg() {
        let mut top = Text::new("a-b");
        top.args = vec![("count".into(), Arg::Int(1)), ("count".into(), Arg::Int(2))];
        let err = serde_json::to_string(&top).unwrap_err().to_string();
        assert!(err.contains("`count`"), "{err}");

        let mut inner = Text::new("c-d");
        inner.args = vec![("kind".into(), Arg::Int(1)), ("kind".into(), Arg::Int(2))];
        let nested = Text::new("a-b").with("x", Arg::List(vec![Arg::Text(Box::new(inner))]));
        let err = serde_json::to_string(&nested).unwrap_err().to_string();
        assert!(err.contains("`kind`"), "{err}");
    }

    #[test]
    #[cfg(not(debug_assertions))]
    fn release_builds_fall_back_to_verbatim_for_non_finite_numbers() {
        assert_eq!(Arg::num_or_verbatim(f64::INFINITY), Arg::Verbatim("inf".into()));
        assert_eq!(Arg::qty_or_verbatim(f64::NAN, Unit::Cm), Arg::Verbatim("NaN cm".into()));
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "must be finite")]
    fn debug_builds_panic_on_a_non_finite_num() {
        let _ = Arg::num_or_verbatim(f64::INFINITY);
    }

    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "must be finite")]
    fn debug_builds_panic_on_a_non_finite_qty() {
        let _ = Arg::qty_or_verbatim(f64::NAN, Unit::Cm);
    }

    #[test]
    fn serde_round_trips_and_keeps_argument_order() {
        let text = sample();
        let back: Text = serde_json::from_str(&serde_json::to_string(&text).unwrap()).unwrap();
        assert_eq!(back, text);
        assert_eq!(back.args.iter().map(|(n, _)| &**n).collect::<Vec<_>>(), ["part", "count", "len", "kind", "all"]);
    }

    #[test]
    fn bad_json_is_refused() {
        for bad in [
            json!({"args": {}}),
            json!({"id": "a", "extra": 1}),
            json!({"id": "a", "args": {"x": {"nope": 1}}}),
            json!({"id": "a", "args": {"x": {"qty": {"value": 1.0, "unit": "furlong"}}}}),
        ] {
            assert!(serde_json::from_value::<Text>(bad.clone()).is_err(), "{bad}");
        }
        let twice = r#"{"id":"a","args":{"x":{"int":1},"x":{"int":2}}}"#;
        assert!(serde_json::from_str::<Text>(twice).is_err());
    }

    #[test]
    fn the_json_schema_describes_text_and_every_arg_variant() {
        let mut generator = schemars::generate::SchemaSettings::draft2020_12().into_generator();
        let root = serde_json::to_value(generator.root_schema_for::<Text>()).unwrap();
        assert_eq!(root["type"], "object");
        assert_eq!(root["required"], json!(["id"]));
        assert_eq!(root["properties"]["id"]["type"], "string");
        assert!(root["properties"]["args"]["additionalProperties"].get("$ref").is_some(), "{root}");
        let defs = &root["$defs"];
        let arg = serde_json::to_string(&defs["Arg"]).unwrap();
        for tag in ["id", "int", "num", "qty", "text", "list", "verbatim"] {
            assert!(arg.contains(&format!("\"required\":[\"{tag}\"]")), "{tag} missing from {arg}");
        }
        let unit = serde_json::to_string(&defs["Unit"]).unwrap();
        for u in ["m", "cm", "mm", "deg", "ms", "s", "percent", "px", "hz", "db"] {
            assert!(unit.contains(&format!("\"{u}\"")), "{u} missing from {unit}");
        }
    }

    #[test]
    fn the_last_resort_display_shows_id_and_arguments() {
        let text = Text::new("char-part-not-found").with("part", Arg::Id("hair-b".into())).with("n", Arg::Int(2));
        assert_eq!(text.to_string(), "⟦char-part-not-found: part=hair-b, n=2⟧");
        assert_eq!(Text::new("a-b").to_string(), "⟦a-b⟧");
    }

    #[test]
    fn message_converts_from_english_and_text() {
        assert_eq!(Message::from("plain"), Message::English("plain".into()));
        assert_eq!(Message::from(String::from("owned")), Message::English("owned".into()));
        assert_eq!(Message::from(Text::new("a-b")), Message::Text(Text::new("a-b")));
    }
}
