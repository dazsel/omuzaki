//! Character styles: a face design and the head and body numbers that go with it, tuned together and applied in one
//! step (`character.set_style`, or `character.create` with a `style`).
//!
//! A look preset ([`look`](crate::look)) changes how a character is *shaded*; a style changes how it is *drawn*: eye
//! shape and lashes, mouth and nose, the face shape, the neck and the body's proportions. The kit's own design is the
//! default; `mature` turns it into an older anime character (narrow eyes under heavy upper lashes, a small mouth, a
//! longer face and neck) on the `standard` and `tall` templates.
//!
//! **Adding a style is adding a file** (`library/styles/<id>.toml`; `build.rs` embeds every file there, so no list in
//! code names them):
//!
//! ```toml
//! id = "mature"                      # the file's stem
//! name = "Mature"
//! summary = "..."
//! origin = "..."                     # where the numbers come from (a design target, never a traced likeness)
//! templates = ["standard", "tall"]   # the templates it is tuned for; others are refused
//! profile = "long"                   # optional: a face-shape preset applied first (`face.set_profile`)
//! [face]                             # merged over the character's face design, field by field
//! [head]                             # merged over the head part's parameters
//! [body]                             # merged over the body part's parameters
//! [[parts]]                          # only when creating: merged over the starter's part of that id, or added
//! ```
//!
//! Merging is field by field for tables and whole for anything else, as the starter's ([`merge`]); the result is checked
//! by the face's and the part kinds' own validation, so a style can never store what an edit could not.

use std::sync::OnceLock;

use omuzaki_char_api::{CharacterDef, HeadParams, PartId, find_kind};
use omuzaki_doc_api::Value;
use omuzaki_face_api::{FaceProfile, Validate};
use serde::Deserialize;

use crate::head::presets::ShapeProfiles;

include!(concat!(env!("OUT_DIR"), "/styles.rs"));

/// One part a style brings when a character is created with it.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StylePart {
    /// The part's id: the starter's part of that id is merged over, or a new part is added.
    pub id: String,
    /// Its kind.
    pub kind: String,
    /// Merged over the starter part's parameters (or the kind's defaults for a new part).
    #[serde(default)]
    pub params: Value,
}

/// A character style, as its file holds it.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Style {
    /// The style's id, its file's stem.
    pub id: String,
    /// The name pickers show.
    pub name: String,
    /// One line: what it looks like.
    pub summary: String,
    /// Where its numbers come from.
    pub origin: String,
    /// The body templates it is tuned for.
    pub templates: Vec<String>,
    /// A face-shape preset applied before the tables below.
    #[serde(default)]
    pub profile: Option<String>,
    /// Merged over the face design.
    #[serde(default)]
    pub face: Value,
    /// Merged over the head part's parameters.
    #[serde(default)]
    pub head: Value,
    /// Merged over the body part's parameters.
    #[serde(default)]
    pub body: Value,
    /// The parts it brings to a new character.
    #[serde(default)]
    pub parts: Vec<StylePart>,
    /// The starter's parts a new character of this style goes without.
    #[serde(default)]
    pub without: Vec<String>,
}

/// Why a style could not be read or applied.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum StyleError {
    /// A style file doesn't read.
    #[error("library/styles/{file}.toml: {message}")]
    File {
        /// The file's stem.
        file: String,
        /// What is wrong.
        message: String,
    },
    /// No style has this id.
    #[error("there is no style `{0}`")]
    Unknown(String),
    /// The character's template is not one the style is tuned for.
    #[error("the `{style}` style is tuned for the {templates} templates, not `{template}`")]
    Template {
        /// The style.
        style: String,
        /// The character's template.
        template: String,
        /// The templates it is tuned for, listed.
        templates: String,
    },
    /// The character has no enabled part of a kind the style changes.
    #[error("the character has no {0} part for the style to change")]
    Missing(&'static str),
    /// The merged result is not valid.
    #[error("{0}")]
    Invalid(String),
}

/// `over` merged into `base`: objects field by field, anything else (a list, a number) replaced whole. An absent or
/// `null` table changes nothing.
pub fn merge(base: &mut Value, over: &Value) {
    match (base, over) {
        (Value::Object(base), Value::Object(over)) => {
            for (key, value) in over {
                match base.get_mut(key) {
                    Some(slot) => merge(slot, value),
                    None if !value.is_null() => {
                        base.insert(key.clone(), value.clone());
                    }
                    None => {}
                }
            }
        }
        (_, Value::Null) => {}
        (base, over) => *base = over.clone(),
    }
}

/// Parses one style file: its `id` must be its stem, and its profile (if any) a known face-shape preset.
///
/// # Errors
/// [`StyleError::File`] naming the file and what is wrong.
pub fn parse(stem: &str, text: &str) -> Result<Style, StyleError> {
    let fail = |message: String| StyleError::File { file: stem.to_owned(), message };
    let style: Style = toml::from_str(text).map_err(|e| fail(e.to_string()))?;
    if style.id != stem {
        return Err(fail(format!("it says it is `{}`: a style's id is its file name", style.id)));
    }
    if style.templates.is_empty() {
        return Err(fail("it names no templates".to_owned()));
    }
    if let Some(profile) = &style.profile {
        ShapeProfiles::get(profile).map_err(|e| fail(format!("profile: {e}")))?;
    }
    Ok(style)
}

/// Every style the crate ships, parsed once, in id order.
///
/// # Errors
/// The first file that doesn't read (the crate's tests rule that out).
pub fn styles() -> Result<&'static [Style], StyleError> {
    static STYLES: OnceLock<Result<Vec<Style>, StyleError>> = OnceLock::new();
    STYLES.get_or_init(|| FILES.iter().map(|(stem, text)| parse(stem, text)).collect()).as_deref().map_err(Clone::clone)
}

/// The style called `id`.
///
/// # Errors
/// [`StyleError::Unknown`], or a file that doesn't read.
pub fn find(id: &str) -> Result<&'static Style, StyleError> {
    styles()?.iter().find(|s| s.id == id).ok_or_else(|| StyleError::Unknown(id.to_owned()))
}

impl Style {
    /// Applies the style to `def`: the face-shape profile, then the face, head and body tables, each checked as an edit
    /// would be. Parts are left as they are ([`Style::parts`] are for new characters).
    ///
    /// # Errors
    /// [`StyleError::Template`] on a template the style isn't tuned for, [`StyleError::Missing`] without a head or a
    /// body, [`StyleError::Invalid`] if a merged result is refused.
    pub fn apply(&self, def: &mut CharacterDef) -> Result<(), StyleError> {
        if !self.templates.iter().any(|t| t == def.template.as_str()) {
            return Err(StyleError::Template {
                style: self.id.clone(),
                template: def.template.to_string(),
                templates: self.templates.join(" and "),
            });
        }
        let head_id = enabled_part(def, "head").ok_or(StyleError::Missing("head"))?;
        let body_id = enabled_part(def, "body").ok_or(StyleError::Missing("body"))?;
        let invalid = |what: &str, e: &dyn std::fmt::Display| StyleError::Invalid(format!("{what}: {e}"));

        let mut face = def.face.clone();
        let mut head_value = def.parts[&head_id].params.clone();
        if let Some(id) = &self.profile {
            let preset = ShapeProfiles::get(id).map_err(|e| invalid("profile", &e))?;
            let mut head: HeadParams = serde_json::from_value(head_value).map_err(|e| invalid("head", &e))?;
            head.outline = preset.profile.outline;
            preset.profile.cranium.unwrap_or_default().apply(&mut head.cranium);
            head.preset = Some(preset.id.clone());
            face.layout = preset.profile.layout;
            head_value = serde_json::to_value(&head).map_err(|e| invalid("head", &e))?;
        }
        let mut face_value = serde_json::to_value(&face).map_err(|e| invalid("face", &e))?;
        merge(&mut face_value, &self.face);
        let face: FaceProfile = serde_json::from_value(face_value).map_err(|e| invalid("face", &e))?;
        face.validate().map_err(|e| invalid("face", &e))?;

        merge(&mut head_value, &self.head);
        let head_value = checked("head", &head_value)?;
        let mut body_value = def.parts[&body_id].params.clone();
        merge(&mut body_value, &self.body);
        let body_value = checked("body", &body_value)?;

        def.face = face;
        if let Some(part) = def.parts.get_mut(&head_id) {
            part.params = head_value;
        }
        if let Some(part) = def.parts.get_mut(&body_id) {
            part.params = body_value;
        }
        Ok(())
    }
}

/// The first enabled part of `kind`.
fn enabled_part(def: &CharacterDef, kind: &str) -> Option<PartId> {
    def.parts.iter().find(|(_, p)| p.enabled && p.kind.as_str() == kind).map(|(id, _)| id.clone())
}

/// `value` checked and normalised by the `kind` part kind.
fn checked(kind: &'static str, value: &Value) -> Result<Value, StyleError> {
    let registration = find_kind(kind).ok_or(StyleError::Missing(kind))?;
    (registration.check)(value).map_err(|e| StyleError::Invalid(format!("{kind}: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_style_file_reads_and_is_named_for_its_id() {
        let all = styles().unwrap();
        assert_eq!(all.len(), FILES.len());
        assert!(find("mature").is_ok());
        assert_eq!(find("nope").unwrap_err(), StyleError::Unknown("nope".into()));
        let text = "id = \"x\"\nname = \"X\"\nsummary = \"s\"\norigin = \"o\"\ntemplates = [\"standard\"]\n";
        assert!(parse("x", text).is_ok());
        assert!(parse("y", text).unwrap_err().to_string().contains("its file name"));
        let bad = text.replace("templates = [\"standard\"]", "templates = []");
        assert!(parse("x", &bad).unwrap_err().to_string().contains("no templates"));
        let profile = format!("{text}profile = \"square\"\n");
        assert!(parse("x", &profile).unwrap_err().to_string().contains("profile"));
    }

    #[test]
    fn merge_goes_field_by_field_and_replaces_lists_whole() {
        let mut base = serde_json::json!({"a": {"b": 1, "c": 2}, "l": [1, 2]});
        merge(&mut base, &serde_json::json!({"a": {"c": 3}, "l": [9], "n": null}));
        assert_eq!(base, serde_json::json!({"a": {"b": 1, "c": 3}, "l": [9]}));
    }
}
