//! The persona (`ai-performer.md` §4, AP-01): who the performer is, as `characters/<id>/persona.toml` holds it.
//!
//! [`Persona::from_toml`] checks the limits; [`Persona::revision`] is the BLAKE3 of the canonical TOML, first 12 hex
//! characters, stamped on every transcript line; [`Persona::render`] is the prompt's persona block, a pure function
//! with fixed English headings and the values verbatim.

use std::fmt::Write as _;

use omuzaki_performer_api::ids::PersonaRevision;
use omuzaki_performer_api::limits::{
    PERSONA_BLOCK_TOKENS, PERSONA_FIELD_CHARS, PERSONA_LIST_ITEMS, PERSONA_REVISION_HEX, PERSONA_SAMPLES,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// What the persona keeps away from.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Boundaries {
    /// Topics it never discusses.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub never: Vec<String>,
    /// Topics it steers away from.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub avoid: Vec<String>,
    /// Topics it answers with a fixed redirect.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub redirect: Vec<Redirect>,
}

/// `{ topic = "spoilers", to = "ask chat to use spoiler tags" }`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Redirect {
    /// The topic.
    pub topic: String,
    /// What to do instead.
    pub to: String,
}

/// One few-shot exchange.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Sample {
    /// What the person says.
    pub user: String,
    /// How the persona answers, tags and all.
    pub reply: String,
}

/// A persona.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Persona {
    /// What the performer answers to; also its first alias.
    pub name: String,
    /// Other names it answers to, including the ASR misspellings the person adds.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub aliases: Vec<String>,
    /// BCP 47 languages; the first is the default reply language.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub languages: Vec<String>,
    /// Its personality.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub personality: String,
    /// Its backstory.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub backstory: String,
    /// How it talks.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub style: String,
    /// Lines it likes to say.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub catchphrases: Vec<String>,
    /// The life layer's personality preset (PF-24).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub life: Option<String>,
    /// Its voice id (AP-02 owns voices and consent).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub voice: Option<String>,
    /// What it keeps away from.
    #[serde(default, skip_serializing_if = "is_default")]
    pub boundaries: Boundaries,
    /// Few-shot exchanges.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub samples: Vec<Sample>,
}

fn is_default(b: &Boundaries) -> bool {
    *b == Boundaries::default()
}

/// Why a persona was refused.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum PersonaError {
    /// The TOML doesn't parse or has unknown keys.
    #[error("persona.toml: {0}")]
    Parse(String),
    /// A limit is crossed.
    #[error("persona: {0}")]
    Limit(String),
}

impl Persona {
    /// A persona named `name`, everything else empty.
    pub fn named(name: impl Into<String>) -> Self {
        Self { name: name.into(), ..Self::default() }
    }

    /// Parses and checks `persona.toml`.
    ///
    /// # Errors
    /// [`PersonaError`] for bad TOML or a crossed limit.
    pub fn from_toml(text: &str) -> Result<Self, PersonaError> {
        let persona: Self = toml::from_str(text).map_err(|e| PersonaError::Parse(e.to_string()))?;
        persona.check()?;
        Ok(persona)
    }

    /// The canonical TOML: what the revision hashes and `persona.get` returns.
    ///
    /// # Errors
    /// [`PersonaError::Parse`] if it can't be written (never, for a persona that parsed).
    pub fn to_toml(&self) -> Result<String, PersonaError> {
        toml::to_string(self).map_err(|e| PersonaError::Parse(e.to_string()))
    }

    /// Checks every limit (§4): each text ≤ 1,000 characters, lists ≤ 16 items, samples ≤ 6, a name, and the rendered
    /// block ≤ 2,000 tokens.
    ///
    /// # Errors
    /// [`PersonaError::Limit`] naming the first one crossed.
    pub fn check(&self) -> Result<(), PersonaError> {
        let limit = |what: &str| Err(PersonaError::Limit(what.to_owned()));
        if self.name.trim().is_empty() {
            return limit("a persona needs a name");
        }
        let texts = [&self.name, &self.personality, &self.backstory, &self.style];
        let lists =
            [&self.aliases, &self.languages, &self.catchphrases, &self.boundaries.never, &self.boundaries.avoid];
        if texts.iter().any(|t| t.chars().count() > PERSONA_FIELD_CHARS)
            || lists.iter().flat_map(|l| l.iter()).any(|t| t.chars().count() > PERSONA_FIELD_CHARS)
            || self
                .samples
                .iter()
                .any(|s| s.user.chars().count() > PERSONA_FIELD_CHARS || s.reply.chars().count() > PERSONA_FIELD_CHARS)
            || self
                .boundaries
                .redirect
                .iter()
                .any(|r| r.topic.chars().count() > PERSONA_FIELD_CHARS || r.to.chars().count() > PERSONA_FIELD_CHARS)
        {
            return limit(&format!("each text is at most {PERSONA_FIELD_CHARS} characters"));
        }
        if lists.iter().any(|l| l.len() > PERSONA_LIST_ITEMS) || self.boundaries.redirect.len() > PERSONA_LIST_ITEMS {
            return limit(&format!("each list has at most {PERSONA_LIST_ITEMS} items"));
        }
        if self.samples.len() > PERSONA_SAMPLES {
            return limit(&format!("at most {PERSONA_SAMPLES} samples"));
        }
        let tokens = estimate_tokens(&self.render());
        if tokens > PERSONA_BLOCK_TOKENS {
            return limit(&format!("the persona block is {tokens} tokens, over {PERSONA_BLOCK_TOKENS}"));
        }
        Ok(())
    }

    /// The BLAKE3 of the canonical TOML, first 12 hex characters.
    pub fn revision(&self) -> PersonaRevision {
        let text = self.to_toml().unwrap_or_default();
        let hex = blake3::hash(text.as_bytes()).to_hex();
        let short = &hex.as_str()[..PERSONA_REVISION_HEX];
        // Twelve lower-case hex digits always make a valid revision.
        PersonaRevision::new(short).unwrap_or_else(|_| unreachable!("12 hex digits"))
    }

    /// The names it answers to: the name, then the aliases.
    pub fn names(&self) -> Vec<String> {
        std::iter::once(self.name.clone()).chain(self.aliases.iter().cloned()).collect()
    }

    /// The prompt's persona block: fixed English headings, values verbatim, samples as exchanges.
    pub fn render(&self) -> String {
        let mut out = String::new();
        let _ = writeln!(out, "# Who you are\nYour name is {}.", self.name);
        if !self.aliases.is_empty() {
            let _ = writeln!(out, "You also answer to: {}.", self.aliases.join(", "));
        }
        if let Some(lang) = self.languages.first() {
            let _ = writeln!(out, "Reply in the language with tag {lang} unless the person uses another.");
        }
        for (heading, text) in
            [("Personality", &self.personality), ("Backstory", &self.backstory), ("How you talk", &self.style)]
        {
            if !text.is_empty() {
                let _ = writeln!(out, "\n## {heading}\n{text}");
            }
        }
        if !self.catchphrases.is_empty() {
            let _ = writeln!(out, "\n## Catchphrases\n{}", self.catchphrases.join("\n"));
        }
        let b = &self.boundaries;
        if !b.never.is_empty() {
            let _ = writeln!(out, "\n## Never discuss\n{}", b.never.join("\n"));
        }
        if !b.avoid.is_empty() {
            let _ = writeln!(out, "\n## Steer away from\n{}", b.avoid.join("\n"));
        }
        if !b.redirect.is_empty() {
            let _ = writeln!(out, "\n## Redirects");
            for r in &b.redirect {
                let _ = writeln!(out, "{}: {}", r.topic, r.to);
            }
        }
        if !self.samples.is_empty() {
            let _ = writeln!(out, "\n## Example exchanges");
            for s in &self.samples {
                let _ = writeln!(out, "Person: {}\nYou: {}", s.user, s.reply);
            }
        }
        out
    }
}

/// The seam's local token estimate: about four characters a token, rounded up.
pub fn estimate_tokens(text: &str) -> u64 {
    (text.chars().count() as u64).div_ceil(4)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// §4's file: a synthetic test persona.
    pub(crate) const MOCHI: &str = r#"
name = "Mochi"
aliases = ["mochi-chan", "mochie"]
languages = ["en"]
personality = "Curious, warm, a little smug about puzzles."
backstory = "A paper-craft fox who lives on the streamer's desk."
style = "Short sentences. Playful. Never more than three sentences unless asked."
catchphrases = ["Fold it and see!"]
life = "energetic"
voice = "voices/mochi"
[boundaries]
never = ["real people's private lives", "medical or legal advice"]
avoid = ["politics"]
redirect = [{ topic = "spoilers", to = "ask chat to use spoiler tags" }]
[[samples]]
user = "Mochi, can you wave?"
reply = "[gesture wave] Hi hi! Fold it and see!"
"#;

    #[test]
    fn the_worked_example_parses_renders_and_has_a_stable_revision() {
        let p = Persona::from_toml(MOCHI).unwrap();
        assert_eq!(p.names(), ["Mochi", "mochi-chan", "mochie"]);
        let block = p.render();
        assert!(block.starts_with("# Who you are\nYour name is Mochi."));
        assert!(block.contains("## Never discuss\nreal people's private lives"));
        assert!(block.contains("Person: Mochi, can you wave?\nYou: [gesture wave] Hi hi! Fold it and see!"));
        let rev = p.revision();
        assert_eq!(rev.as_str().len(), 12);
        // The same persona written differently (key order, spacing) has the same revision.
        let again = Persona::from_toml(&p.to_toml().unwrap()).unwrap();
        assert_eq!(again.revision(), rev);
        let mut changed = p.clone();
        changed.style.push('!');
        assert_ne!(changed.revision(), rev);
    }

    #[test]
    fn limits_are_enforced() {
        assert!(matches!(Persona::from_toml("name = \"\""), Err(PersonaError::Limit(_))));
        assert!(matches!(Persona::from_toml("name = \"A\"\nfoo = 1"), Err(PersonaError::Parse(_))));
        let long = format!("name = \"A\"\npersonality = \"{}\"", "x".repeat(PERSONA_FIELD_CHARS + 1));
        assert!(matches!(Persona::from_toml(&long), Err(PersonaError::Limit(_))));
        let many = format!("name = \"A\"\naliases = [{}]", vec!["\"a\""; PERSONA_LIST_ITEMS + 1].join(","));
        assert!(matches!(Persona::from_toml(&many), Err(PersonaError::Limit(_))));
        let samples =
            "name = \"A\"\n".to_owned() + &"[[samples]]\nuser = \"u\"\nreply = \"r\"\n".repeat(PERSONA_SAMPLES + 1);
        assert!(matches!(Persona::from_toml(&samples), Err(PersonaError::Limit(_))));
        // Within each field's limit, the block as a whole can still be too long.
        let mut big = Persona::named("A");
        big.catchphrases = vec!["y".repeat(PERSONA_FIELD_CHARS); PERSONA_LIST_ITEMS];
        assert!(matches!(big.check(), Err(PersonaError::Limit(m)) if m.contains("tokens")));
    }
}
