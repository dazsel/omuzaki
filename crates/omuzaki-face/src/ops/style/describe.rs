//! `face.describe_style`: the schema, defaults and current values of the face sections, so an agent can read what a field
//! holds before it sets one.

use omuzaki_char_api::CharId;
use omuzaki_face_api::{BrowStyle, CheekStyle, EyeStyle, FaceMark, FaceProfile, MouthStyle, NoseStyle};
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use schemars::generate::SchemaSettings;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use crate::ops::shared::edit::{effective, stored_face, to_json};
use crate::ops::shared::path::{SECTIONS, section};

/// Describes the face sections of a character: for each, its path, its JSON Schema (inlined), its defaults and its current
/// values. Marks are a list: `defaults` is empty and `item_defaults` is what a new mark starts from.
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
            "ranges": "not reported yet; a refused value names the range it accepts",
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
            }
            entry
        }
    };
    Ok(entry)
}

/// The entry for a section that is one struct: its schema, its defaults and the values it has now.
fn section_entry<T: JsonSchema + Default + Serialize>(path: &str, current: &T) -> Result<Value, OpError> {
    Ok(json!({
        "path": path,
        "schema": schema_of::<T>()?,
        "defaults": to_json(&T::default())?,
        "current": to_json(current)?,
    }))
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
