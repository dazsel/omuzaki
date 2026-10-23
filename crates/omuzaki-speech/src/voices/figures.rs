//! The impersonation warning (§3.5): names of public figures whose voices are widely imitated, shipped as data
//! (`public-figures.toml`). A match warns; it never blocks (users own their use), and a voice that really is a cloned
//! person's needs that person's consent record whatever its name.

use std::sync::OnceLock;

use serde::Deserialize;

/// The shipped list.
const SHIPPED: &str = include_str!("public-figures.toml");

/// One public figure: the name to show and the other names that match too.
#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Figure {
    /// The name, as the warning shows it.
    pub name: String,
    /// Other names that match.
    #[serde(default)]
    pub aliases: Vec<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    figures: Vec<Figure>,
}

/// A list of figures with each name pre-split into words.
#[derive(Clone, Debug)]
pub struct PublicFigures {
    figures: Vec<(Figure, Vec<Vec<String>>)>,
}

impl PublicFigures {
    /// The list in `toml` (the shipped format: `[[figures]]` with `name` and `aliases`).
    ///
    /// # Errors
    /// The parse error, in words.
    pub fn parse(toml: &str) -> Result<Self, String> {
        let file: File = toml::from_str(toml).map_err(|e| e.to_string())?;
        let figures = file
            .figures
            .into_iter()
            .map(|figure| {
                let names = std::iter::once(&figure.name).chain(&figure.aliases).map(|n| words(n)).collect();
                (figure, names)
            })
            .collect();
        Ok(Self { figures })
    }

    /// The shipped list, parsed once.
    pub fn shipped() -> &'static Self {
        static SHIPPED_LIST: OnceLock<PublicFigures> = OnceLock::new();
        // The list is checked by a test; a broken edit would show as an empty list, never a panic.
        SHIPPED_LIST.get_or_init(|| Self::parse(SHIPPED).unwrap_or(Self { figures: Vec::new() }))
    }

    /// How many figures it holds.
    pub fn len(&self) -> usize {
        self.figures.len()
    }

    /// Whether it holds none.
    pub fn is_empty(&self) -> bool {
        self.figures.is_empty()
    }

    /// The figure `text` names, if any: one of its names as whole words of `text`, in order, or run together as one
    /// word (`MorganFreeman`, `morgan_freeman`).
    pub fn find(&self, text: &str) -> Option<&Figure> {
        let text = words(text);
        if text.is_empty() {
            return None;
        }
        self.figures
            .iter()
            .find(|(_, names)| {
                names.iter().any(|name| {
                    !name.is_empty()
                        && (text.windows(name.len()).any(|w| w == name.as_slice())
                            || text.iter().any(|word| *word == name.concat()))
                })
            })
            .map(|(figure, _)| figure)
    }
}

/// `text` as lowercase words: letters and digits, accents dropped, split at anything else and at a lowercase-to-
/// uppercase step (`MorganFreeman` is two words).
fn words(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut word = String::new();
    let mut previous_lower = false;
    for c in text.chars() {
        let c = fold(c);
        if c.is_alphanumeric() {
            if c.is_uppercase() && previous_lower && !word.is_empty() {
                out.push(std::mem::take(&mut word));
            }
            previous_lower = c.is_lowercase();
            word.extend(c.to_lowercase());
        } else {
            previous_lower = false;
            if !word.is_empty() {
                out.push(std::mem::take(&mut word));
            }
        }
    }
    if !word.is_empty() {
        out.push(word);
    }
    out
}

/// The Latin letter without its accent, for the accents names in the list are written without.
fn fold(c: char) -> char {
    match c {
        'à' | 'á' | 'â' | 'ã' | 'ä' | 'å' => 'a',
        'À' | 'Á' | 'Â' | 'Ã' | 'Ä' | 'Å' => 'A',
        'è' | 'é' | 'ê' | 'ë' => 'e',
        'È' | 'É' | 'Ê' | 'Ë' => 'E',
        'ì' | 'í' | 'î' | 'ï' => 'i',
        'Ì' | 'Í' | 'Î' | 'Ï' => 'I',
        'ò' | 'ó' | 'ô' | 'õ' | 'ö' => 'o',
        'Ò' | 'Ó' | 'Ô' | 'Õ' | 'Ö' => 'O',
        'ù' | 'ú' | 'û' | 'ü' => 'u',
        'Ù' | 'Ú' | 'Û' | 'Ü' => 'U',
        'ñ' => 'n',
        'Ñ' => 'N',
        'ç' => 'c',
        'Ç' => 'C',
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shipped_list_parses_and_matches_whole_words_in_any_spelling() {
        let list = PublicFigures::parse(SHIPPED).unwrap();
        assert!(list.len() >= 25, "{}", list.len());
        let name = |text: &str| list.find(text).map(|f| f.name.clone());
        for text in ["Morgan Freeman", "morgan_freeman", "MorganFreeman", "my-morgan-freeman-voice", "morganfreeman"] {
            assert_eq!(name(text).as_deref(), Some("Morgan Freeman"), "{text}");
        }
        assert_eq!(name("Beyoncé").as_deref(), Some("Beyonce"));
        assert_eq!(name("zelensky_v2").as_deref(), Some("Volodymyr Zelenskyy"));
        for text in ["kokoro-82m/af_heart", "Mochi", "freeman", "morgan", "", "a cheerful narrator"] {
            assert_eq!(name(text), None, "{text}");
        }
    }

    #[test]
    fn a_broken_list_says_why() {
        assert!(PublicFigures::parse("[[figures]]\nnmae = \"x\"\n").is_err());
        assert!(PublicFigures::parse("").is_err());
    }
}
