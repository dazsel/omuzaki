//! The words the brief knows, and what each one does to a character: `library/lexicon.toml`, read once.
//!
//! The lexicon is data. A **facet** is one thing a description can say (the build, the hair style, the eyes, the
//! outfit, accessories...); its **options** each list the phrases that choose it and what choosing it does, as the
//! kit's own ops: a body template or style for `character.create`, parts to add and parts they replace, and op steps.
//! **Colours** are named colours with the hex they stand for; **targets** are the nouns a colour can go to on their own
//! (`hair`, `eyes`, `outfit`). **Priors** are what an unnamed facet or colour is drawn from, by the seed. Adding a word
//! or an option is adding lines to the file: the tests check that every option's steps plan on a fresh character.

use std::collections::BTreeSet;
use std::sync::OnceLock;

use omuzaki_core::colour::Colour;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::parse::Reading;

/// The built-in lexicon.
const LEXICON: &str = include_str!("../library/lexicon.toml");

/// Where a colour goes.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ColourTarget {
    /// The hair's gradient (`hair.recolour`).
    Hair,
    /// The irises (`face.set_style` on `eyes/iris`).
    Eyes,
    /// The outfit's main garment: the first part the outfit adds, or the kit's shirt.
    Outfit,
    /// A part, by id, when it is there.
    Part(String),
}

/// A named colour.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ColourWord {
    /// Its id, as the vocabulary and `facets` take it.
    pub id: String,
    /// The phrases that name it.
    pub words: Vec<String>,
    /// The colour.
    pub hex: Colour,
    /// The hair's root, middle and tip for it, when the derived gradient doesn't read as this colour of hair.
    #[serde(default)]
    pub hair: Option<[Colour; 3]>,
}

/// A noun a colour can go to on its own.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TargetWords {
    /// Where the colour goes.
    pub target: ColourTarget,
    /// The phrases.
    pub words: Vec<String>,
}

/// One thing a description can say.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Facet {
    /// Its id.
    pub id: String,
    /// One line on what it is.
    pub summary: String,
    /// Several options at once (accessories), rather than one.
    #[serde(default)]
    pub many: bool,
    /// What it is when the description doesn't say: options drawn by the seed (empty: the kit's default stays).
    #[serde(default)]
    pub prior: Vec<String>,
    /// Facets whose values this one may change on purpose (makeup's blush over the skin tone's cheeks), so the
    /// self-check reads it as a refinement, not a conflict.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub refines: Vec<String>,
}

/// A part an option adds.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PartSpec {
    /// The part's id.
    pub id: String,
    /// Its kind.
    pub kind: String,
    /// What it changes of the kind's defaults, merged in with `part.set_param` after `part.add`.
    #[serde(default)]
    pub params: Option<Value>,
    /// Where it is fixed, as `part.add` takes it (a tie on the shirt's `collar_ring`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attach: Option<Value>,
}

/// An op step, with `$character` standing for the character's id.
#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Step {
    /// The op's name.
    pub op: String,
    /// Its arguments.
    pub args: Value,
}

/// One option of a facet.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Choice {
    /// The facet it belongs to.
    pub facet: String,
    /// Its id within the facet.
    pub id: String,
    /// One line on what it gives.
    pub summary: String,
    /// The phrases that choose it.
    pub words: Vec<String>,
    /// Phrases that hint at it (`mage` for a robe): they choose it only when nothing stronger in the description
    /// chooses for the same facet or part.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub hints: Vec<String>,
    /// The body template it creates the character on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template: Option<String>,
    /// The character style it creates the character with (`character.list_styles`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub style: Option<String>,
    /// The templates it needs (the mature style is for `standard` and `tall`); empty: any.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fits: Vec<String>,
    /// Its words count only next to a thing of this kind (`long` only in `long silver hair`, never `a long dress`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub near: Option<ColourTarget>,
    /// Parts it removes when the character has them (the kit outfit's skirt, for trousers).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub replaces: Vec<String>,
    /// Parts it adds (or, when the character has one by that id, sets the parameters of).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parts: Vec<PartSpec>,
    /// Op steps it runs, in order, after its parts.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub steps: Vec<Step>,
    /// The outfit colours it suggests (`gothic`: black, maroon, purple), by colour id: an unnamed outfit colour is drawn
    /// from these instead of the usual priors.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tones: Vec<String>,
    /// Options it doesn't go with, as `facet.option`: a prior is never drawn against what the description chose
    /// (`bare legs` avoids the outfits with trousers).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub avoids: Vec<String>,
    /// Where a colour named with it goes (`a red dress`): `outfit`, `hair`, or a part's id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub colour: Option<ColourTarget>,
}

/// What an unnamed colour is drawn from.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ColourPriors {
    /// Hair colours, by colour id.
    pub hair: Vec<String>,
    /// Eye colours.
    pub eyes: Vec<String>,
    /// The outfit's main colour.
    pub outfit: Vec<String>,
}

/// The whole lexicon.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Lexicon {
    /// The facets, in the order their steps run.
    pub facet: Vec<Facet>,
    /// Every option.
    pub option: Vec<Choice>,
    /// The named colours.
    pub colour: Vec<ColourWord>,
    /// The nouns that take a colour.
    pub target: Vec<TargetWords>,
    /// What unnamed colours are drawn from.
    pub priors: ColourPriors,
    /// Every phrase, split into words, with what it reads as.
    #[serde(skip)]
    phrases: Vec<(Vec<String>, Reading)>,
}

impl Lexicon {
    /// The built-in lexicon.
    ///
    /// # Errors
    /// The file doesn't parse, or names a facet, option or colour that isn't there (a bug in the file; a test reads
    /// it).
    pub fn builtin() -> Result<&'static Self, String> {
        static BUILT: OnceLock<Result<Lexicon, String>> = OnceLock::new();
        BUILT.get_or_init(|| Self::parse(LEXICON)).as_ref().map_err(Clone::clone)
    }

    /// A lexicon from `text`, checked.
    ///
    /// # Errors
    /// See [`builtin`](Self::builtin).
    pub fn parse(text: &str) -> Result<Self, String> {
        let mut lexicon: Self = toml::from_str(text).map_err(|e| format!("library/lexicon.toml: {e}"))?;
        lexicon.check()?;
        lexicon.phrases = lexicon.index();
        Ok(lexicon)
    }

    /// The facet `id`.
    pub fn facet(&self, id: &str) -> Option<&Facet> {
        self.facet.iter().find(|f| f.id == id)
    }

    /// Option `id` of `facet`.
    pub fn option(&self, facet: &str, id: &str) -> Option<&Choice> {
        self.option.iter().find(|o| o.facet == facet && o.id == id)
    }

    /// The options of `facet`, in the file's order.
    pub fn options<'a>(&'a self, facet: &'a str) -> impl Iterator<Item = &'a Choice> {
        self.option.iter().filter(move |o| o.facet == facet)
    }

    /// The named colour `id`.
    pub fn colour(&self, id: &str) -> Option<&ColourWord> {
        self.colour.iter().find(|c| c.id == id)
    }

    /// The named colour that is exactly `colour`, if any.
    pub fn colour_named(&self, colour: Colour) -> Option<&ColourWord> {
        self.colour.iter().find(|c| c.hex == colour)
    }

    /// Every phrase with what it reads as.
    pub fn phrases(&self) -> impl Iterator<Item = (&[String], &Reading)> {
        self.phrases.iter().map(|(words, reading)| (words.as_slice(), reading))
    }

    fn index(&self) -> Vec<(Vec<String>, Reading)> {
        let split = |phrase: &str| -> Vec<String> { crate::parse::words(phrase).into_iter().map(|w| w.text).collect() };
        let mut out = Vec::new();
        for c in &self.colour {
            out.extend(c.words.iter().map(|w| (split(w), Reading::Colour(c.hex))));
        }
        for t in &self.target {
            out.extend(t.words.iter().map(|w| (split(w), Reading::Target(t.target.clone()))));
        }
        for o in &self.option {
            let reading = |hint| Reading::Choice { facet: o.facet.clone(), option: o.id.clone(), hint };
            out.extend(o.words.iter().map(|w| (split(w), reading(false))));
            out.extend(o.hints.iter().map(|w| (split(w), reading(true))));
        }
        out
    }

    fn check(&self) -> Result<(), String> {
        let facets: BTreeSet<&str> = self.facet.iter().map(|f| f.id.as_str()).collect();
        if facets.len() != self.facet.len() {
            return Err("library/lexicon.toml: two facets share an id".into());
        }
        let mut options = BTreeSet::new();
        for o in &self.option {
            if !facets.contains(o.facet.as_str()) {
                return Err(format!("library/lexicon.toml: option `{}` names the unknown facet `{}`", o.id, o.facet));
            }
            if !options.insert((o.facet.as_str(), o.id.as_str())) {
                return Err(format!("library/lexicon.toml: `{}` is in facet `{}` twice", o.id, o.facet));
            }
            if o.words.is_empty() && o.hints.is_empty() {
                return Err(format!("library/lexicon.toml: option `{}.{}` has no words", o.facet, o.id));
            }
        }
        for o in &self.option {
            let unknown = o
                .avoids
                .iter()
                .find(|a| a.split_once('.').is_none_or(|(facet, option)| self.option(facet, option).is_none()));
            if let Some(missing) = unknown {
                return Err(format!(
                    "library/lexicon.toml: `{}.{}` avoids the unknown option `{missing}`",
                    o.facet, o.id
                ));
            }
        }
        for f in &self.facet {
            if let Some(missing) = f.refines.iter().find(|r| !facets.contains(r.as_str())) {
                return Err(format!("library/lexicon.toml: facet `{}` refines the unknown facet `{missing}`", f.id));
            }
            if let Some(missing) = f.prior.iter().find(|p| self.option(&f.id, p).is_none()) {
                return Err(format!(
                    "library/lexicon.toml: facet `{}`'s prior `{missing}` is not one of its options",
                    f.id
                ));
            }
        }
        let tones = self.option.iter().flat_map(|o| &o.tones);
        let priors = self.priors.hair.iter().chain(&self.priors.eyes).chain(&self.priors.outfit).chain(tones);
        if let Some(missing) = priors.clone().find(|p| self.colour(p).is_none()) {
            return Err(format!("library/lexicon.toml: the colour prior `{missing}` is not a named colour"));
        }
        let mut seen = BTreeSet::new();
        let words = self
            .colour
            .iter()
            .flat_map(|c| &c.words)
            .chain(self.target.iter().flat_map(|t| &t.words))
            .chain(self.option.iter().flat_map(|o| o.words.iter().chain(&o.hints)));
        for w in words {
            if !seen.insert(w.to_lowercase()) {
                return Err(format!("library/lexicon.toml: the phrase `{w}` means two things"));
            }
        }
        Ok(())
    }
}
