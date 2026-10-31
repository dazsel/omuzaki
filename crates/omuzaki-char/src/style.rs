//! Character styles: a whole art style, applied in one step (`character.set_style`, `character.mix_styles`, or
//! `character.create` with a `style`).
//!
//! A style is how a character is *drawn* and how it is *shaded*, tuned together: the face design (eye shape, lashes,
//! iris and catchlights, mouth, nose, brows), the face shape, the head and body numbers (neck, proportions, and through
//! its templates how many heads tall), the hairstyle, the look preset that shades it (lines, terminator, grade) and
//! where it sits on the shading style axis (`look.set_style`: `0` physical, `1` cel). The kit's own design is the
//! default; the library spans anime of several kinds (shoujo, shonen, a 90s cel look, chibi, mature), Western animation
//! (the animated feature film, the comic book) and the realistic end (semi-real, high fidelity), each family listed by
//! `character.list_styles`.
//!
//! **Styles own their fields and never compound.** Every field any style sets belongs to the styles: applying one sets
//! the fields it names and returns the other style-owned fields to the kit's default character on the same template,
//! so shoujo after chibi is exactly shoujo, whatever came before. Fields no style names (colours, marks, the
//! expressions, every other part) are left as they are.
//!
//! **Styles blend.** A style applies by an `amount` (`0` leaves the character as it is, `1` is the full style), and
//! several styles mix by weight: every number is the weighted mean of what each style would set, and every discrete
//! choice (a nose kind, the template, the hairstyle, the look preset) follows the heaviest entry. So the styles are
//! points in one continuous space, and any point between them is a character.
//!
//! **Adding a style is adding a file** (`library/styles/<id>.toml`; `build.rs` embeds every file there, so no list in
//! code names them):
//!
//! ```toml
//! id = "mature"                      # the file's stem
//! name = "Mature"
//! family = "anime"                   # how pickers group it: anime, western, real, ...
//! summary = "..."
//! origin = "..."                     # where the numbers come from (a design target, never a traced likeness)
//! templates = ["standard", "tall"]   # the templates it is tuned for; the first is the one it moves a character to
//! profile = "long"                   # optional: a face-shape preset applied first (`face.set_profile`)
//! hair = "bob"                       # optional: a hairstyle (`hair.style.apply`) the style brings
//! look = "soft-sd"                   # optional: the look preset that shades it (`look.set_preset`)
//! shading = 1.0                      # optional: the style axis for the whole character, 0 physical to 1 cel
//! [face]                             # merged over the character's face design, field by field
//! [head]                             # merged over the head part's parameters
//! [body]                             # merged over the body part's parameters
//! [[parts]]                          # only when creating: merged over the starter's part of that id, or added
//! ```
//!
//! Merging is field by field for tables and whole for anything else, as the starter's ([`merge`]); the result is checked
//! by the face's and the part kinds' own validation, so a style can never store what an edit could not.

use std::collections::BTreeSet;
use std::sync::OnceLock;

use omuzaki_char_api::{BodyTemplateId, CharacterDef, HairParams, HeadParams, LookDef, LookPresetId, PartId, find_kind};
use omuzaki_doc_api::Value;
use omuzaki_face_api::{FaceProfile, Validate};
use serde::Deserialize;
use serde_json::json;

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
    /// The family pickers group it under: `anime`, `western`, `real` (`other` when the file names none).
    #[serde(default = "other")]
    pub family: String,
    /// One line: what it looks like.
    pub summary: String,
    /// Where its numbers come from.
    pub origin: String,
    /// The body templates it is tuned for; the first is the one `character.set_style` moves a character to.
    pub templates: Vec<String>,
    /// A face-shape preset applied before the tables below.
    #[serde(default)]
    pub profile: Option<String>,
    /// A hairstyle of the library (`hair.style.apply`) the style cuts the hair to.
    #[serde(default)]
    pub hair: Option<String>,
    /// The look preset that shades the style.
    #[serde(default)]
    pub look: Option<String>,
    /// Where the style sits on the shading style axis, `0` physical to `1` cel, for the whole character.
    #[serde(default)]
    pub shading: Option<f64>,
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

/// The family of a style file that names none.
fn other() -> String {
    "other".to_owned()
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
    /// The weights of a mix are not usable.
    #[error("{0}")]
    Weights(String),
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

/// The weighted mean of `values` (weights summing to 1, none zero): numbers are averaged (and rounded when every one
/// is a whole number, so counts stay counts), tables blend key by key, lists of one length element by element, and
/// anything else (a string, a flag, lists of different lengths, a key some entries lack) is the `lead` entry's.
pub fn blend(values: &[(&Value, f64)], lead: usize) -> Value {
    let pick = || values.get(lead).or_else(|| values.first()).map_or(Value::Null, |(v, _)| (*v).clone());
    if values.len() < 2 {
        return pick();
    }
    if values.iter().all(|(v, _)| v.is_number()) {
        let mean: f64 = values.iter().map(|(v, w)| v.as_f64().unwrap_or(0.0) * w).sum();
        if values.iter().all(|(v, _)| v.is_u64()) {
            return json!(whole(mean).max(0));
        }
        if values.iter().all(|(v, _)| v.is_i64() || v.is_u64()) {
            return json!(whole(mean));
        }
        return json!(mean);
    }
    if let Some(objects) = values.iter().map(|(v, w)| v.as_object().map(|o| (o, *w))).collect::<Option<Vec<_>>>() {
        let keys: BTreeSet<&String> = objects.iter().flat_map(|(o, _)| o.keys()).collect();
        let mut out = serde_json::Map::new();
        for key in keys {
            let present: Option<Vec<(&Value, f64)>> = objects.iter().map(|(o, w)| o.get(key).map(|v| (v, *w))).collect();
            let value = match present {
                Some(present) => blend(&present, lead),
                None => match objects.get(lead).and_then(|(o, _)| o.get(key)) {
                    Some(v) => v.clone(),
                    None => continue,
                },
            };
            out.insert(key.clone(), value);
        }
        return Value::Object(out);
    }
    if let Some(lists) = values.iter().map(|(v, w)| v.as_array().map(|a| (a, *w))).collect::<Option<Vec<_>>>()
        && let Some(len) = lists.first().map(|(a, _)| a.len())
        && lists.iter().all(|(a, _)| a.len() == len)
    {
        let items = (0..len).map(|i| blend(&lists.iter().map(|(a, w)| (&a[i], *w)).collect::<Vec<_>>(), lead));
        return Value::Array(items.collect());
    }
    pick()
}

/// `x` rounded to the nearest whole number, saturating at the range of `i64`.
#[allow(clippy::cast_possible_truncation, reason = "rounded and clamped to the i64 range first")]
fn whole(x: f64) -> i64 {
    #[allow(clippy::cast_precision_loss, reason = "the bounds only clamp; exactness at 2^63 doesn't matter")]
    let (lo, hi) = (i64::MIN as f64, i64::MAX as f64);
    x.round().clamp(lo, hi) as i64
}

/// Parses one style file: its `id` must be its stem, its profile (if any) a known face-shape preset, its hairstyle
/// one of the library's and its shading on the axis.
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
    if let Some(hair) = &style.hair {
        crate::hair::styles::find(hair).map_err(|e| fail(format!("hair: {e}")))?;
    }
    if let Some(shading) = style.shading
        && !(0.0..=1.0).contains(&shading)
    {
        return Err(fail(format!("shading: {shading} is outside 0 (physical) to 1 (cel)")));
    }
    if let Some(look) = &style.look {
        LookPresetId::new(look).map_err(|e| fail(format!("look: {e}")))?;
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

// ------------------------------------------------------------------------------------------------ owned fields

/// A field's path inside a table, key by key.
type FieldPath = Vec<String>;

/// The fields of the face, head and body tables any style sets, and whether any style brings a face-shape profile:
/// what styles own and reset to the kit's default when a style leaves them out.
#[derive(Default)]
struct Owned {
    face: BTreeSet<FieldPath>,
    head: BTreeSet<FieldPath>,
    body: BTreeSet<FieldPath>,
    profile: bool,
    hair: bool,
}

/// The leaf paths of `value`: every key path that ends at something that isn't a table.
fn leaves(value: &Value, prefix: &mut FieldPath, out: &mut BTreeSet<FieldPath>) {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                prefix.push(key.clone());
                leaves(child, prefix, out);
                prefix.pop();
            }
        }
        Value::Null => {}
        _ if prefix.is_empty() => {}
        _ => {
            out.insert(prefix.clone());
        }
    }
}

/// The leaf paths of one table.
fn leaf_set(value: &Value) -> BTreeSet<FieldPath> {
    let mut out = BTreeSet::new();
    leaves(value, &mut Vec::new(), &mut out);
    out
}

/// What every shipped style owns, gathered once.
fn owned() -> &'static Owned {
    static OWNED: OnceLock<Owned> = OnceLock::new();
    OWNED.get_or_init(|| {
        let mut owned = Owned::default();
        for style in styles().unwrap_or_default() {
            owned.face.extend(leaf_set(&style.face));
            owned.head.extend(leaf_set(&style.head));
            owned.body.extend(leaf_set(&style.body));
            owned.profile |= style.profile.is_some();
            owned.hair |= style.hair.is_some();
        }
        owned
    })
}

/// The value at `path` in `value`, if every key along it is there.
fn get<'a>(value: &'a Value, path: &[String]) -> Option<&'a Value> {
    path.iter().try_fold(value, |v, key| v.get(key))
}

/// Sets `path` in `value` to `to` (creating tables along the way), or removes it when `to` is `None`.
fn set(value: &mut Value, path: &[String], to: Option<&Value>) {
    let Some((last, along)) = path.split_last() else { return };
    let mut at = value;
    for key in along {
        let Some(map) = at.as_object_mut() else { return };
        if !map.contains_key(key) {
            if to.is_none() {
                return;
            }
            map.insert(key.clone(), Value::Object(serde_json::Map::new()));
        }
        let Some(next) = map.get_mut(key) else { return };
        at = next;
    }
    let Some(map) = at.as_object_mut() else { return };
    match to {
        Some(v) => {
            map.insert(last.clone(), v.clone());
        }
        None => {
            map.remove(last);
        }
    }
}

/// Every path of `owned` that `mine` doesn't set, in `value`, back to what `neutral` holds there. Where `neutral` has
/// nothing (or `null`) part way along a path, that whole table goes back: an optional table a style added (a sculpted
/// nose) is removed, not left empty.
fn reset(value: &mut Value, neutral: &Value, owned: &BTreeSet<FieldPath>, mine: &BTreeSet<FieldPath>) {
    for path in owned.difference(mine) {
        let end = (1..=path.len())
            .find(|&i| get(neutral, &path[..i]).is_none_or(Value::is_null))
            .unwrap_or(path.len());
        set(value, &path[..end], get(neutral, &path[..end]));
    }
}

// ------------------------------------------------------------------------------------------------ applying

/// What a restyle may change besides the face, head and body.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub struct Options {
    /// Cut the hair to the leading style's hairstyle.
    pub hair: bool,
    /// Shade with the leading style's look preset and the mix's place on the style axis.
    pub look: bool,
    /// Refuse a template the leading style isn't tuned for, instead of moving the character to its first template.
    pub keep_template: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self { hair: true, look: true, keep_template: false }
    }
}

impl Options {
    /// The options with the hair, look and template choices given.
    pub const fn new(hair: bool, look: bool, keep_template: bool) -> Self {
        Self { hair, look, keep_template }
    }
}

/// A restyled character: its definition (the template, face and parts) and its look, when the restyle changed it.
#[derive(Clone, Debug)]
#[non_exhaustive]
pub struct Restyled {
    /// The definition after the restyle.
    pub def: CharacterDef,
    /// The look node after the restyle; `None` when it is left as it was.
    pub look: Option<LookDef>,
    /// The style the discrete choices followed (`None`: the character as it was).
    pub lead: Option<&'static Style>,
}

/// `def` (shaded by `look`) restyled by a weighted mix: each entry is a style, or `None` for the character as it is,
/// with a weight `>= 0`. One style at weight 1 is the full style; `[(None, 1 - a), (style, a)]` applies it by `a`.
///
/// # Errors
/// [`StyleError::Weights`] for no positive weight or a weight that isn't a finite number `>= 0`,
/// [`StyleError::Template`] when `keep_template` is set and the lead isn't tuned for the template, and the errors of
/// applying a style.
pub fn restyle(
    def: &CharacterDef,
    look: &LookDef,
    mix: &[(Option<&'static Style>, f64)],
    options: Options,
) -> Result<Restyled, StyleError> {
    if let Some((_, w)) = mix.iter().find(|(_, w)| !(w.is_finite() && *w >= 0.0)) {
        return Err(StyleError::Weights(format!("a weight of {w} is not a number from 0 up")));
    }
    let total: f64 = mix.iter().map(|(_, w)| w).sum();
    if total <= 0.0 {
        return Err(StyleError::Weights("the weights add up to 0: give at least one a weight above 0".to_owned()));
    }
    let entries: Vec<(Option<&'static Style>, f64)> =
        mix.iter().filter(|(_, w)| *w > 0.0).map(|(s, w)| (*s, w / total)).collect();
    let lead_index = entries
        .iter()
        .enumerate()
        .max_by(|(_, a), (_, b)| a.1.total_cmp(&b.1))
        .map_or(0, |(i, _)| i);
    let lead = entries.get(lead_index).and_then(|(s, _)| *s);

    let kit = |template: &BodyTemplateId| {
        crate::ops::character::create::default_character(template)
            .map_err(|e| StyleError::Invalid(format!("the kit's default character: {}", e.message)))
    };
    let mut base = def.clone();
    if let Some(style) = lead
        && !style.tuned_for(base.template.as_str())
    {
        if options.keep_template {
            return Err(style.template_error(base.template.as_str()));
        }
        let first = style.templates.first().map_or("standard", String::as_str);
        let template = BodyTemplateId::new(first).map_err(|e| StyleError::Invalid(format!("template: {e}")))?;
        base = moved(&base, &kit(&base.template)?, &kit(&template)?, template)?;
    }
    if entries.iter().all(|(s, _)| s.is_none()) {
        return Ok(Restyled { def: base, look: None, lead });
    }
    let neutral = kit(&base.template)?;

    let results: Vec<CharacterDef> = entries
        .iter()
        .map(|(style, _)| match style {
            Some(style) => {
                let mut one = base.clone();
                let leads = lead.is_some_and(|l| std::ptr::eq(l, *style));
                style.apply_over(&mut one, &neutral, options.hair && leads)?;
                Ok(one)
            }
            None => Ok(base.clone()),
        })
        .collect::<Result<_, StyleError>>()?;

    let invalid = |what: &str, e: &dyn std::fmt::Display| StyleError::Invalid(format!("{what}: {e}"));
    let faces: Vec<Value> =
        results.iter().map(|r| serde_json::to_value(&r.face)).collect::<Result<_, _>>().map_err(|e| invalid("face", &e))?;
    let face: FaceProfile =
        serde_json::from_value(blend(&weighted(&faces, &entries), lead_index)).map_err(|e| invalid("face", &e))?;
    face.validate().map_err(|e| invalid("face", &e))?;

    let mut out = results.get(lead_index).cloned().unwrap_or_else(|| base.clone());
    out.face = face;
    for kind in ["head", "body"] {
        let Some(id) = enabled_part(&base, kind) else { continue };
        let values: Vec<Value> = results.iter().map(|r| r.parts.get(&id).map_or(Value::Null, |p| p.params.clone())).collect();
        let blended = checked(kind, &blend(&weighted(&values, &entries), lead_index))?;
        if let Some(part) = out.parts.get_mut(&id) {
            part.params = blended;
        }
    }

    let look = if options.look { restyled_look(look, &entries, lead)? } else { None };
    Ok(Restyled { def: out, look, lead })
}

/// `def` moved to `template`: every face, head and body field still at the old template's default (`from`, the kit's
/// default character there) takes the new template's (`to`), so the neck, the head and the face layout follow the
/// template unless they were edited.
fn moved(
    def: &CharacterDef,
    from: &CharacterDef,
    to: &CharacterDef,
    template: BodyTemplateId,
) -> Result<CharacterDef, StyleError> {
    let invalid = |what: &str, e: &dyn std::fmt::Display| StyleError::Invalid(format!("{what}: {e}"));
    let mut out = def.clone();
    out.template = template;
    let mut face = serde_json::to_value(&def.face).map_err(|e| invalid("face", &e))?;
    let (old, new) = (
        serde_json::to_value(&from.face).map_err(|e| invalid("face", &e))?,
        serde_json::to_value(&to.face).map_err(|e| invalid("face", &e))?,
    );
    rebase(&mut face, &old, &new);
    out.face = serde_json::from_value(face).map_err(|e| invalid("face", &e))?;
    for kind in ["head", "body"] {
        let part = |d: &CharacterDef| enabled_part(d, kind).and_then(|id| d.parts.get(&id)).map(|p| p.params.clone());
        if let (Some(id), Some(old), Some(new)) = (enabled_part(def, kind), part(from), part(to))
            && let Some(slot) = out.parts.get_mut(&id)
        {
            rebase(&mut slot.params, &old, &new);
            slot.params = checked(kind, &slot.params)?;
        }
    }
    Ok(out)
}

/// Every leaf of `value` equal to `old`'s at the same path set to `new`'s there (tables walked key by key).
fn rebase(value: &mut Value, old: &Value, new: &Value) {
    match (value, old, new) {
        (Value::Object(map), Value::Object(old), Value::Object(new)) => {
            for (key, slot) in map.iter_mut() {
                if let (Some(o), Some(n)) = (old.get(key), new.get(key)) {
                    rebase(slot, o, n);
                }
            }
        }
        (slot, old, new) => {
            if slot == old {
                *slot = new.clone();
            }
        }
    }
}

/// Each of `values` with the weight of the mix entry it came from.
fn weighted<'a>(values: &'a [Value], entries: &[(Option<&'static Style>, f64)]) -> Vec<(&'a Value, f64)> {
    values.iter().zip(entries).map(|(v, (_, w))| (v, *w)).collect()
}

/// The look after a mix. The look preset and the style axis are style-owned like the fields: the preset is the lead
/// style's (the default look for a lead that names none), and the axis is the weighted mean of the entries' (the
/// character's own for an entry that is the character as it is, cel for a style that names none), cleared when no
/// entry sets one. `None` when the character as it is leads every choice and no style takes part.
fn restyled_look(
    look: &LookDef,
    entries: &[(Option<&'static Style>, f64)],
    lead: Option<&'static Style>,
) -> Result<Option<LookDef>, StyleError> {
    let invalid = |e: &dyn std::fmt::Display| StyleError::Invalid(format!("look: {e}"));
    if entries.iter().all(|(s, _)| s.is_none()) {
        return Ok(None);
    }
    let mut after = look.clone();
    if let Some(style) = lead {
        after.preset = style.look.as_deref().map(LookPresetId::new).transpose().map_err(|e| invalid(&e))?;
    }
    let now = look.overrides.all.style.map(f64::from);
    let axes: Vec<(Option<f64>, f64)> =
        entries.iter().map(|(s, w)| (s.map_or(now, |s| s.shading), *w)).collect();
    let axis = if axes.iter().all(|(a, _)| a.is_none()) {
        None
    } else {
        let mean: f64 = axes.iter().map(|(a, w)| a.unwrap_or(1.0) * w).sum();
        Some(json!(axis_value(mean)))
    };
    after.overrides.set_param("all.style", axis).map_err(|e| invalid(&e))?;
    after.resolve().map_err(|e| invalid(&e))?;
    Ok(Some(after))
}

/// A place on the style axis as the look stores it.
#[allow(clippy::cast_possible_truncation, reason = "a value in 0..=1 fits an f32")]
fn axis_value(x: f64) -> f32 {
    x.clamp(0.0, 1.0) as f32
}

impl Style {
    /// Whether the style is tuned for `template`.
    pub fn tuned_for(&self, template: &str) -> bool {
        self.templates.iter().any(|t| t == template)
    }

    /// The error for a template the style isn't tuned for.
    fn template_error(&self, template: &str) -> StyleError {
        StyleError::Template {
            style: self.id.clone(),
            template: template.to_owned(),
            templates: self.templates.join(" and "),
        }
    }

    /// Applies the whole style to `def` on its own template (as `character.create` does): the face, head and body,
    /// and the hairstyle. The look is the caller's ([`restyle`] gives it).
    ///
    /// # Errors
    /// [`StyleError::Template`] on a template the style isn't tuned for, and the errors of [`restyle`].
    pub fn apply(&'static self, def: &mut CharacterDef) -> Result<(), StyleError> {
        if !self.tuned_for(def.template.as_str()) {
            return Err(self.template_error(def.template.as_str()));
        }
        // The hair of a new character is the starter's with the style's own parts merged in (`character.create`), so
        // only a style with a hairstyle recuts it.
        let options = Options { look: false, hair: self.hair.is_some(), keep_template: false };
        *def = restyle(def, &LookDef::default(), &[(Some(self), 1.0)], options)?.def;
        Ok(())
    }

    /// Applies the style to `def` over `neutral` (the kit's default character on the same template): the style-owned
    /// fields it doesn't name go back to `neutral`'s, then the face-shape profile, the face, head and body tables, and
    /// (with `hair`) the hairstyle, each checked as an edit would be. Other parts are left as they are.
    fn apply_over(&self, def: &mut CharacterDef, neutral: &CharacterDef, hair: bool) -> Result<(), StyleError> {
        let head_id = enabled_part(def, "head").ok_or(StyleError::Missing("head"))?;
        let body_id = enabled_part(def, "body").ok_or(StyleError::Missing("body"))?;
        let invalid = |what: &str, e: &dyn std::fmt::Display| StyleError::Invalid(format!("{what}: {e}"));
        let owned = owned();
        let neutral_part = |kind: &str| {
            enabled_part(neutral, kind).and_then(|id| neutral.parts.get(&id)).map_or(Value::Null, |p| p.params.clone())
        };
        let (neutral_head, neutral_body) = (neutral_part("head"), neutral_part("body"));
        let neutral_face = serde_json::to_value(&neutral.face).map_err(|e| invalid("face", &e))?;

        let mut face_value = serde_json::to_value(&def.face).map_err(|e| invalid("face", &e))?;
        let mut head_value = def.parts[&head_id].params.clone();
        let mut body_value = def.parts[&body_id].params.clone();
        reset(&mut face_value, &neutral_face, &owned.face, &leaf_set(&self.face));
        reset(&mut head_value, &neutral_head, &owned.head, &leaf_set(&self.head));
        reset(&mut body_value, &neutral_body, &owned.body, &leaf_set(&self.body));
        if owned.profile && self.profile.is_none() {
            for key in ["outline", "cranium", "preset"] {
                set(&mut head_value, &[key.to_owned()], neutral_head.get(key));
            }
            set(&mut face_value, &["layout".to_owned()], neutral_face.get("layout"));
        }

        if let Some(id) = &self.profile {
            let preset = ShapeProfiles::get(id).map_err(|e| invalid("profile", &e))?;
            let mut head: HeadParams = serde_json::from_value(head_value).map_err(|e| invalid("head", &e))?;
            head.outline = preset.profile.outline;
            let mut cranium = serde_json::from_value(neutral_head.get("cranium").cloned().unwrap_or(Value::Null))
                .unwrap_or_else(|_| head.cranium.clone());
            preset.profile.cranium.unwrap_or_default().apply(&mut cranium);
            head.cranium = cranium;
            head.preset = Some(preset.id.clone());
            face_value["layout"] = serde_json::to_value(preset.profile.layout).map_err(|e| invalid("face", &e))?;
            head_value = serde_json::to_value(&head).map_err(|e| invalid("head", &e))?;
        }
        merge(&mut face_value, &self.face);
        let face: FaceProfile = serde_json::from_value(face_value).map_err(|e| invalid("face", &e))?;
        face.validate().map_err(|e| invalid("face", &e))?;

        merge(&mut head_value, &self.head);
        let head_value = checked("head", &head_value)?;
        merge(&mut body_value, &self.body);
        let body_value = checked("body", &body_value)?;

        def.face = face;
        if let Some(part) = def.parts.get_mut(&head_id) {
            part.params = head_value;
        }
        if let Some(part) = def.parts.get_mut(&body_id) {
            part.params = body_value;
        }
        if hair
            && let Some(hair_id) = enabled_part(def, "hair")
            && let Some(part) = def.parts.get_mut(&hair_id)
        {
            let mut params: HairParams =
                serde_json::from_value(part.params.clone()).map_err(|e| invalid("hair", &e))?;
            if let Some(cut) = &self.hair {
                params = crate::hair::styles::find(cut).map_err(|e| invalid("hair", &e))?.applied_to(&params);
            } else if owned.hair
                && let Some(neutral) = enabled_part(neutral, "hair").and_then(|id| neutral.parts.get(&id))
            {
                // The hairstyle is style-owned too: a style without one gives back the kit's cut (colours kept).
                let kit: HairParams =
                    serde_json::from_value(neutral.params.clone()).map_err(|e| invalid("hair", &e))?;
                params = HairParams { groups: kit.groups, volume: kit.volume, ..params };
            }
            part.params = checked("hair", &serde_json::to_value(params).map_err(|e| invalid("hair", &e))?)?;
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
        let text = "id = \"x\"\nname = \"X\"\nfamily = \"anime\"\nsummary = \"s\"\norigin = \"o\"\ntemplates = [\"standard\"]\n";
        assert!(parse("x", text).is_ok());
        assert!(parse("y", text).unwrap_err().to_string().contains("its file name"));
        let bad = text.replace("templates = [\"standard\"]", "templates = []");
        assert!(parse("x", &bad).unwrap_err().to_string().contains("no templates"));
        let profile = format!("{text}profile = \"square\"\n");
        assert!(parse("x", &profile).unwrap_err().to_string().contains("profile"));
        let hair = format!("{text}hair = \"mohawk-spikes\"\n");
        assert!(parse("x", &hair).unwrap_err().to_string().contains("hair"));
        let shading = format!("{text}shading = 1.5\n");
        assert!(parse("x", &shading).unwrap_err().to_string().contains("shading"));
    }

    #[test]
    fn merge_goes_field_by_field_and_replaces_lists_whole() {
        let mut base = serde_json::json!({"a": {"b": 1, "c": 2}, "l": [1, 2]});
        merge(&mut base, &serde_json::json!({"a": {"c": 3}, "l": [9], "n": null}));
        assert_eq!(base, serde_json::json!({"a": {"b": 1, "c": 3}, "l": [9]}));
    }

    #[test]
    fn blend_averages_numbers_and_takes_the_lead_for_the_rest() {
        let a = json!({"x": 0.0, "n": 10, "s": "dot", "l": [0.0, 2.0], "m": [1], "only_a": true});
        let b = json!({"x": 1.0, "n": 21, "s": "tick", "l": [1.0, 4.0], "m": [1, 2]});
        let mixed = blend(&[(&a, 0.25), (&b, 0.75)], 1);
        assert_eq!(mixed["x"], json!(0.75));
        assert_eq!(mixed["n"], json!(18), "whole numbers stay whole: 10 * 0.25 + 21 * 0.75 = 18.25");
        assert_eq!(mixed["s"], json!("tick"));
        assert_eq!(mixed["l"], json!([0.75, 3.5]));
        assert_eq!(mixed["m"], json!([1, 2]), "lists of different lengths are the lead's");
        assert!(mixed.get("only_a").is_none(), "a key the lead lacks is left out");
        // Weight 1 on one entry is that entry exactly.
        assert_eq!(blend(&[(&b, 1.0)], 0), b);
    }

    #[test]
    fn reset_returns_owned_fields_the_style_leaves_out() {
        let mut value = json!({"eyes": {"scale": 1.4, "lash": {"flick": 0.9}}, "kept": 3});
        let neutral = json!({"eyes": {"scale": 1.0, "lash": {"flick": 0.5}}});
        let owned: BTreeSet<FieldPath> = [vec!["eyes".into(), "scale".into()], vec!["eyes".into(), "lash".into(), "flick".into()]]
            .into_iter()
            .collect();
        let mine = leaf_set(&json!({"eyes": {"scale": 2.0}}));
        reset(&mut value, &neutral, &owned, &mine);
        assert_eq!(value, json!({"eyes": {"scale": 1.4, "lash": {"flick": 0.5}}, "kept": 3}));
        // A path the neutral lacks is removed.
        let mut value = json!({"nose3d": {"size": 2.0}});
        reset(&mut value, &json!({}), &[vec!["nose3d".into(), "size".into()]].into_iter().collect(), &BTreeSet::new());
        assert_eq!(value, json!({}));
        let mut value = json!({"nose3d": {"size": 2.0}});
        reset(&mut value, &json!({"nose3d": null}), &[vec!["nose3d".into(), "size".into()]].into_iter().collect(), &BTreeSet::new());
        assert_eq!(value, json!({"nose3d": null}));
    }
}
