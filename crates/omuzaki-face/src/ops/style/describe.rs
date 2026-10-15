//! `face.describe_style`: the schema, defaults and current values of the face sections, so an agent can read what a field
//! holds before it sets one.

use omuzaki_char_api::CharId;
use omuzaki_face_api::{
    BrowStyle, CheekStyle, EyeStyle, FaceMark, FaceProfile, FieldRange, MouthStyle, NoseStyle, Validate,
};
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use schemars::generate::SchemaSettings;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::ops_shared::edit::{effective, stored_face, to_json};
use crate::ops_shared::path::{SECTIONS, section};

/// Describes the face sections of a character: for each, its path, its JSON Schema (inlined), its defaults, its current
/// values and the `ranges` of its numeric fields (`path`, `lo`, `hi`, and a `note` for a rule between fields). Marks are
/// a list: `defaults` is empty, `item_defaults` is what a new mark starts from and `item_ranges` are one mark's ranges.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DescribeStyle {
    /// The character whose face to describe.
    pub character: CharId,
    /// One section (`eyes`, `brows`, `mouth`, `nose`, `cheeks` or `marks`); all six when omitted.
    #[serde(default)]
    pub section: Option<String>,
}

impl Op for DescribeStyle {
    fn meta() -> OpMeta {
        OpMeta {
            name: "face.describe_style",
            summary: "Describe the face sections of a character: schema, defaults and current values of each field",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let stored = stored_face(ctx, &self.character)?;
        let profile = effective(stored.as_ref())?;
        let wanted: Vec<&str> = match &self.section {
            Some(name) => vec![section(name)?],
            None => SECTIONS.to_vec(),
        };
        let mut sections = Map::new();
        for name in wanted {
            sections.insert(name.to_owned(), describe(name, &profile)?);
        }
        Ok(Plan::answer(json!({
            "character": self.character.to_string(),
            "sections": sections,
        })))
    }
}

register_op!(DescribeStyle);

/// One section's description, for a section name that [`section`] has already accepted.
fn describe(name: &str, profile: &FaceProfile) -> Result<Value, OpError> {
    let path = format!("face/{name}");
    let entry = match name {
        "eyes" => section_entry::<EyeStyle>(&path, &profile.eyes)?,
        "brows" => section_entry::<BrowStyle>(&path, &profile.brows)?,
        "mouth" => section_entry::<MouthStyle>(&path, &profile.mouth)?,
        "nose" => section_entry::<NoseStyle>(&path, &profile.nose)?,
        "cheeks" => section_entry::<CheekStyle>(&path, &profile.cheeks)?,
        _ => {
            let mut entry = list_entry(&path, &profile.marks)?;
            if let Value::Object(fields) = &mut entry {
                fields.insert("item_defaults".to_owned(), to_json(&FaceMark::default())?);
                fields.insert("item_ranges".to_owned(), ranges_of(&format!("{path}/<i>"), &FaceMark::default()));
            }
            entry
        }
    };
    Ok(entry)
}

/// The entry for a section that is one struct: its schema, its defaults, the values it has now and its ranges.
fn section_entry<T: JsonSchema + Default + Serialize + Validate>(path: &str, current: &T) -> Result<Value, OpError> {
    Ok(json!({
        "path": path,
        "schema": schema_of::<T>()?,
        "defaults": to_json(&T::default())?,
        "current": to_json(current)?,
        "ranges": ranges_of(path, current),
    }))
}

/// The ranges of `item`'s numeric fields, each with the full path an agent sets it by below `path`.
fn ranges_of(path: &str, item: &impl Validate) -> Value {
    Value::Array(item.ranges().iter().map(|r| range_entry(path, r)).collect())
}

/// One range as the entry `describe_style` lists: `{path, lo, hi}`, plus `note` when a rule between fields applies.
fn range_entry(path: &str, range: &FieldRange) -> Value {
    let mut entry = json!({ "path": format!("{path}/{}", range.path), "lo": range.lo, "hi": range.hi });
    if let (Some(note), Value::Object(fields)) = (&range.note, &mut entry) {
        fields.insert("note".to_owned(), Value::from(note.as_str()));
    }
    entry
}

/// The entry for the marks list: its schema and the marks it holds now, with no defaults of its own.
fn list_entry(path: &str, current: &[FaceMark]) -> Result<Value, OpError> {
    Ok(json!({
        "path": path,
        "schema": schema_of::<Vec<FaceMark>>()?,
        "defaults": [],
        "current": to_json(current)?,
    }))
}

/// The JSON Schema of `T`, with every sub-schema inlined so an agent reads it in one place.
fn schema_of<T: JsonSchema>() -> Result<Value, OpError> {
    let schema = SchemaSettings::draft2020_12()
        .with(|s| s.inline_subschemas = true)
        .into_generator()
        .into_root_schema_for::<T>();
    to_json(&schema)
}
