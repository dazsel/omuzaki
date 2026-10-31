//! From what was read to the kit's ops: which option each facet takes and why, the `character.create` arguments, and
//! the steps after it.
//!
//! **Where each choice comes from**, first that applies: the caller's `facets` (an agent that read the description
//! itself), the description, the facet's prior drawn by the seed, or nothing (the kit's default stays). Colours go the
//! same way: named, or drawn from the lexicon's colour priors by the seed, so two seeds give two different, finished
//! characters for the same words, and one seed always the same one.
//!
//! **The steps** run in the lexicon's facet order (the build, the body, the skin, the outfit, the hair, the face, the
//! accessories), then the colours. An option's parts are added (or, when the character already has a part by that id,
//! changed) and the parts it replaces removed; an option's own steps follow, with `$character` filled in.

use std::collections::BTreeMap;

use omuzaki_core::colour::Colour;
use serde::Serialize;
use serde_json::{Value, json};

use crate::lexicon::{Choice, ColourTarget, Lexicon, Step};
use crate::parse::{Bound, Read};
use crate::shades;

/// The template a character is built on when the description names no build.
pub const DEFAULT_TEMPLATE: &str = "standard";
/// The hair part the kit's default character has, which the hair steps change.
pub const HAIR: &str = "hair";
/// The garment that takes the outfit's colour when no chosen outfit names one: the kit's shirt.
const KIT_TOP: &str = "shirt";

/// Where a choice came from.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    /// The caller's `facets`.
    Facets,
    /// The description's words.
    Description,
    /// Drawn from the lexicon's priors by the seed.
    Prior,
}

/// One choice the brief made.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Made {
    /// The facet (`hair_style`), or the colour's target (`hair_colour`).
    pub facet: String,
    /// The option's id, or the colours (`#c0c8d8`, or `silver`).
    pub value: String,
    /// What it gives, in a line.
    pub summary: String,
    /// The words that chose it, when the description did.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub because: String,
    /// Where it came from.
    pub from: Source,
}

/// What the caller can name directly: a facet's option ids, or a colour target's colours.
pub type Named = BTreeMap<String, Vec<String>>;

/// Everything the brief will do.
#[derive(Clone, Debug)]
pub struct Recipe {
    /// `character.create`'s template.
    pub template: String,
    /// `character.create`'s style, if any.
    pub style: Option<String>,
    /// The choices, in step order.
    pub made: Vec<Made>,
    /// The options chosen, by facet, in step order.
    pub options: Vec<Choice>,
    /// The colours, by target, in step order.
    pub colours: Vec<(ColourTarget, Vec<Colour>)>,
    /// Choices left out because they don't fit another one, as (the words or option, why).
    pub dropped: Vec<(String, String)>,
}

/// A refusal of the caller's `facets`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BadFacet {
    /// The key at fault.
    pub key: String,
    /// What is wrong.
    pub message: String,
}

/// The colour targets `facets` can name, with their keys.
pub const COLOUR_KEYS: [(&str, ColourTarget); 3] =
    [("hair_colour", ColourTarget::Hair), ("eye_colour", ColourTarget::Eyes), ("outfit_colour", ColourTarget::Outfit)];

/// The recipe for `read` and `named`, drawing priors with `seed`.
///
/// # Errors
/// A `named` key that is no facet or colour key, an option a facet doesn't have, a colour that is neither a named one
/// nor `#rrggbb`, or several options for a facet that takes one.
pub fn recipe(lexicon: &Lexicon, read: &Read, named: &Named, seed: u32) -> Result<Recipe, BadFacet> {
    check_named(lexicon, named)?;
    // What the person asked for, before any prior is drawn: a prior never contradicts it (bare legs, then trousers).
    let asked: Vec<(&str, &str)> = named
        .iter()
        .flat_map(|(facet, ids)| ids.iter().map(move |id| (facet.as_str(), id.as_str())))
        .chain(read.picked.iter().map(|p| (p.facet.as_str(), p.option.as_str())))
        .collect();
    let mut made = Vec::new();
    let mut options = Vec::new();
    for (index, facet) in lexicon.facet.iter().enumerate() {
        let chosen: Vec<(String, String, Source)> = if let Some(ids) = named.get(&facet.id) {
            ids.iter().map(|id| (id.clone(), String::new(), Source::Facets)).collect()
        } else {
            let said: Vec<_> = read
                .picked
                .iter()
                .filter(|p| p.facet == facet.id)
                .map(|p| (p.option.clone(), p.because.clone(), Source::Description))
                .collect();
            let prior: Vec<String> =
                facet.prior.iter().filter(|p| !clashes(lexicon, &facet.id, p, &asked)).cloned().collect();
            match (said.is_empty(), draw(&prior, seed, index)) {
                (true, Some(prior)) => vec![(prior.clone(), String::new(), Source::Prior)],
                _ => said,
            }
        };
        for (id, because, from) in chosen {
            let Some(option) = lexicon.option(&facet.id, &id) else { continue };
            made.push(Made { facet: facet.id.clone(), value: id, summary: option.summary.clone(), because, from });
            options.push(option.clone());
        }
    }
    let template = options.iter().find_map(|o| o.template.clone()).unwrap_or_else(|| DEFAULT_TEMPLATE.to_owned());
    let mut dropped = Vec::new();
    let mut kept = 0;
    while kept < options.len() {
        let option = &options[kept];
        if option.fits.is_empty() || option.fits.contains(&template) {
            kept += 1;
            continue;
        }
        let gone = made.remove(kept);
        let words = if gone.because.is_empty() { gone.value } else { gone.because };
        dropped.push((words, format!("it needs the {} build, not {template}", option.fits.join(" or "))));
        options.remove(kept);
    }
    let style = options.iter().find_map(|o| o.style.clone());
    let colours = colours(lexicon, read, named, seed, &options, &mut made);
    Ok(Recipe { template, style, made, options, colours, dropped })
}

/// The steps after `character.create`, for character `id` whose parts after creation are `parts` (id and kind): each
/// option's parts and steps, then the colours. Each step comes with the index in [`Recipe::made`] of the choice it
/// carries out.
pub fn steps(recipe: &Recipe, id: &str, parts: &mut Vec<(String, String)>) -> Vec<(usize, Step)> {
    let mut out = Vec::new();
    for (made, option) in recipe.options.iter().enumerate() {
        let mut own = Vec::new();
        for gone in &option.replaces {
            if let Some(at) = parts.iter().position(|(p, _)| p == gone) {
                parts.remove(at);
                own.push(Step { op: "part.remove".into(), args: json!({"character": id, "part": gone}) });
            }
        }
        for part in &option.parts {
            if parts.iter().any(|(p, _)| *p == part.id) {
                if let Some(Value::Object(params)) = &part.params {
                    own.extend(params.iter().map(|(key, value)| set_param(id, &part.id, key, value)));
                }
            } else {
                let mut args = json!({"character": id, "kind": part.kind, "id": part.id});
                if let Some(params) = &part.params {
                    args["params"] = params.clone();
                }
                if let Some(attach) = &part.attach {
                    args["attach"] = attach.clone();
                }
                own.push(Step { op: "part.add".into(), args });
                parts.push((part.id.clone(), part.kind.clone()));
            }
        }
        own.extend(option.steps.iter().map(|s| Step { op: s.op.clone(), args: fill(&s.args, id) }));
        out.extend(own.into_iter().map(|s| (made, s)));
    }
    for (j, (target, colours)) in recipe.colours.iter().enumerate() {
        let made = recipe.options.len() + j;
        out.extend(colour_steps(target, colours, id, recipe, parts).into_iter().map(|s| (made, s)));
    }
    out
}

/// `part.set_param` on one top-level key: the kind merges an object into what is there.
fn set_param(id: &str, part: &str, path: &str, value: &Value) -> Step {
    Step { op: "part.set_param".into(), args: json!({"character": id, "part": part, "path": path, "value": value}) }
}

/// The steps that give `target` `colours`.
fn colour_steps(
    target: &ColourTarget,
    colours: &[Colour],
    id: &str,
    recipe: &Recipe,
    parts: &[(String, String)],
) -> Vec<Step> {
    let hex = |c: Colour| c.to_string();
    match target {
        ColourTarget::Hair => {
            if !parts.iter().any(|(p, _)| p == HAIR) {
                return Vec::new();
            }
            let named = Lexicon::builtin().ok().and_then(|l| match colours {
                [one] => l.colour_named(*one).and_then(|c| c.hair),
                _ => None,
            });
            let [root, mid, tip] = shades::hair(colours, named);
            vec![Step {
                op: "hair.recolour".into(),
                args: json!({"character": id, "part": HAIR, "root": hex(root), "mid": hex(mid), "tip": hex(tip)}),
            }]
        }
        ColourTarget::Eyes => {
            let [top, mid, low, ring] = shades::iris(colours);
            vec![Step {
                op: "face.set_style".into(),
                args: json!({"character": id, "path": "eyes/iris", "value": {
                    "top": hex(top), "mid": hex(mid), "low": hex(low), "ring": hex(ring)
                }}),
            }]
        }
        ColourTarget::Outfit => colour_part(&main_garment(&recipe.options), colours, id, parts),
        ColourTarget::Part(part) => colour_part(part, colours, id, parts),
    }
}

/// The colour parameters an accessory kind may have, in the order tried: the first one there takes the colour.
const ACCENT_KEYS: [&str; 8] = ["hat", "horn", "wing", "tail", "frame", "cup", "colour", "primary"];

/// The steps that make part `part` `colours`, when it is there: a garment's cloth (a plain cloth takes the colour; a
/// patterned one keeps its pattern in the colour's shades), or an accessory's own colour.
fn colour_part(part: &str, colours: &[Colour], id: &str, parts: &[(String, String)]) -> Vec<Step> {
    let Some((_, kind)) = parts.iter().find(|(p, _)| p == part) else { return Vec::new() };
    let Some(colour) = colours.first().copied() else { return Vec::new() };
    let Some(registration) = omuzaki_char_api::find_kind(kind) else { return Vec::new() };
    let defaults = (registration.defaults)();
    let [base, shade, deep] = shades::cloth(colour);
    let (base, shade, deep) = (base.to_string(), shade.to_string(), deep.to_string());
    if let Some(fabric) = defaults.pointer("/common/fabric") {
        let value = match fabric.get("kind").and_then(Value::as_str) {
            Some("damask") => json!({"kind": "damask", "ground": shade, "figure": base}),
            Some("weave") => json!({"kind": "weave", "warp": base, "weft": shade}),
            Some("stripes") => json!({"kind": "stripes", "ground": base, "stripe": deep}),
            Some("plaid") => json!({"kind": "plaid", "ground": base}),
            _ => json!({"kind": "plain", "colour": base}),
        };
        let mut steps = vec![set_param(id, part, "common/fabric", &value)];
        // A skirt draws its plaid from its own colours: the ground and the bands follow the colour too.
        if defaults.get("plaid").is_some() {
            steps.push(set_param(id, part, "plaid", &json!({"base": base, "band": shade})));
        }
        return steps;
    }
    ACCENT_KEYS
        .iter()
        .find(|key| defaults.get(**key).is_some_and(Value::is_string))
        .map(|key| vec![set_param(id, part, key, &json!(base))])
        .unwrap_or_default()
}

/// The part the outfit's colour goes to: the chosen outfit's own, or the kit's shirt.
fn main_garment(options: &[Choice]) -> String {
    options
        .iter()
        .find_map(|o| match &o.colour {
            Some(ColourTarget::Part(p)) if o.facet == "outfit" => Some(p.clone()),
            _ => None,
        })
        .unwrap_or_else(|| KIT_TOP.to_owned())
}

/// Every colour target with its colours: named in `named`, then in the description, then drawn from the priors.
fn colours(
    lexicon: &Lexicon,
    read: &Read,
    named: &Named,
    seed: u32,
    options: &[Choice],
    made: &mut Vec<Made>,
) -> Vec<(ColourTarget, Vec<Colour>)> {
    let mut out: Vec<(ColourTarget, Vec<Colour>)> = Vec::new();
    // The outfit's colour goes to its main garment, so `a red dress` and an outfit colour never both colour the dress.
    let main = main_garment(options);
    let to_main = ColourTarget::Part(main.clone());
    let mut put = |target: ColourTarget, colours: Vec<Colour>, made_one: Made, made: &mut Vec<Made>| {
        let target = if target == ColourTarget::Outfit { to_main.clone() } else { target };
        if !out.iter().any(|(t, _)| *t == target) {
            out.push((target, colours));
            made.push(made_one);
        }
    };
    for (key, target) in COLOUR_KEYS {
        if let Some(values) = named.get(key) {
            let colours: Vec<Colour> = values.iter().filter_map(|v| colour_value(lexicon, v)).collect();
            put(target, colours.clone(), colour_made(lexicon, key, &colours, "", Source::Facets), made);
        }
    }
    for c in &read.coloured {
        let target = match &c.target {
            Bound::Target(t) => t.clone(),
            Bound::Option { facet, option } => match lexicon.option(facet, option).and_then(|o| o.colour.clone()) {
                Some(t) => t,
                None => continue,
            },
        };
        let key = key_of(&target, &main);
        put(target, c.colours.clone(), colour_made(lexicon, &key, &c.colours, &c.because, Source::Description), made);
    }
    // An option that sets a mood (gothic, a fairy's wings) draws the outfit's colour from its own tones.
    let outfit = options.iter().find(|o| !o.tones.is_empty()).map_or(&lexicon.priors.outfit, |o| &o.tones);
    let priors = [
        (ColourTarget::Hair, &lexicon.priors.hair, 101),
        (ColourTarget::Eyes, &lexicon.priors.eyes, 202),
        (ColourTarget::Outfit, outfit, 303),
    ];
    for (target, prior, salt) in priors {
        // An outfit option without a colour target keeps its own colours (a kimono's pattern).
        if target == ColourTarget::Outfit && options.iter().any(|o| o.facet == "outfit" && o.colour.is_none()) {
            continue;
        }
        let Some(id) = draw(prior, seed, salt) else { continue };
        let Some(colour) = lexicon.colour(id) else { continue };
        let key = key_of(&target, &main);
        put(target, vec![colour.hex], colour_made(lexicon, &key, &[colour.hex], "", Source::Prior), made);
    }
    out
}

/// The `facets` key of a colour target.
fn key_of(target: &ColourTarget, main: &str) -> String {
    match target {
        ColourTarget::Hair => "hair_colour".into(),
        ColourTarget::Eyes => "eye_colour".into(),
        ColourTarget::Outfit => "outfit_colour".into(),
        ColourTarget::Part(p) if p == main => "outfit_colour".into(),
        ColourTarget::Part(p) => format!("{p}_colour"),
    }
}

fn colour_made(lexicon: &Lexicon, key: &str, colours: &[Colour], because: &str, from: Source) -> Made {
    let names: Vec<String> =
        colours.iter().map(|c| lexicon.colour_named(*c).map_or_else(|| c.to_string(), |n| n.id.clone())).collect();
    let summary = match names.as_slice() {
        [one] => one.clone(),
        many => format!("a gradient from {}", many.join(" to ")),
    };
    Made { facet: key.to_owned(), value: names.join(" to "), summary, because: because.to_owned(), from }
}

/// A colour as `facets` names it: a named colour's id or one of its words, or `#rrggbb`.
fn colour_value(lexicon: &Lexicon, value: &str) -> Option<Colour> {
    let value = value.trim().to_lowercase();
    value
        .parse()
        .ok()
        .or_else(|| lexicon.colour.iter().find(|c| c.id == value || c.words.contains(&value)).map(|c| c.hex))
}

fn check_named(lexicon: &Lexicon, named: &Named) -> Result<(), BadFacet> {
    for (key, values) in named {
        let bad = |message: String| BadFacet { key: key.clone(), message };
        if COLOUR_KEYS.iter().any(|(k, _)| k == key) {
            if let Some(v) = values.iter().find(|v| colour_value(lexicon, v).is_none()) {
                return Err(bad(format!(
                    "`{v}` is not a colour: name one (character.brief_vocabulary) or write #rrggbb"
                )));
            }
            continue;
        }
        let Some(facet) = lexicon.facet(key) else {
            let mut keys: Vec<&str> = lexicon.facet.iter().map(|f| f.id.as_str()).collect();
            keys.extend(COLOUR_KEYS.iter().map(|(k, _)| *k));
            return Err(bad(format!("there is no facet `{key}`; the facets are {}", keys.join(", "))));
        };
        if !facet.many && values.len() > 1 {
            return Err(bad(format!("`{key}` takes one option, not {}", values.len())));
        }
        if let Some(v) = values.iter().find(|v| lexicon.option(key, v).is_none()) {
            let ids: Vec<&str> = lexicon.options(key).map(|o| o.id.as_str()).collect();
            return Err(bad(format!("`{key}` has no option `{v}`; it has {}", ids.join(", "))));
        }
    }
    Ok(())
}

/// One of `from`, by `seed` and `salt` (a facet's own stream, so one facet's prior never moves another's).
/// Whether option `id` of `facet` avoids one of `asked`, or one of them avoids it.
fn clashes(lexicon: &Lexicon, facet: &str, id: &str, asked: &[(&str, &str)]) -> bool {
    let named = |f: &str, o: &str| format!("{f}.{o}");
    let this = named(facet, id);
    let avoids = lexicon.option(facet, id).map(|o| o.avoids.as_slice()).unwrap_or_default();
    asked.iter().any(|(f, o)| {
        avoids.contains(&named(f, o)) || lexicon.option(f, o).is_some_and(|other| other.avoids.contains(&this))
    })
}

/// The seed the priors are drawn with: `seed` mixed with the description's words, so two descriptions on the same
/// seed don't draw the same unnamed choices, and the same words on the same seed always do.
pub fn seed_for(description: &str, seed: u32) -> u32 {
    let mut hash: u32 = 0x811c_9dc5;
    for word in crate::parse::words(description) {
        for byte in word.text.bytes().chain(*b" ") {
            hash = (hash ^ u32::from(byte)).wrapping_mul(0x0100_0193);
        }
    }
    hash ^ seed
}

fn draw(from: &[String], seed: u32, salt: usize) -> Option<&String> {
    if from.is_empty() {
        return None;
    }
    let mut x = u64::from(seed) ^ (salt as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15);
    x ^= x >> 33;
    x = x.wrapping_mul(0xff51_afd7_ed55_8ccd);
    x ^= x >> 33;
    x = x.wrapping_mul(0xc4ce_b9fe_1a85_ec53);
    x ^= x >> 33;
    let len = u64::try_from(from.len()).ok()?;
    from.get(usize::try_from(x % len).ok()?)
}

/// `args` with every `"$character"` string replaced by `id`.
fn fill(args: &Value, id: &str) -> Value {
    match args {
        Value::String(s) if s == "$character" => Value::String(id.to_owned()),
        Value::Array(items) => Value::Array(items.iter().map(|v| fill(v, id)).collect()),
        Value::Object(map) => Value::Object(map.iter().map(|(k, v)| (k.clone(), fill(v, id))).collect()),
        other => other.clone(),
    }
}
