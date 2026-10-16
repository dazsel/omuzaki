//! Prompts (recipes, spec v3 §11): named, parameterised instructions an agent's client offers its user.
//!
//! A [`PromptDef`] says which ops it needs. A session is offered only the prompts whose ops all exist in its tool
//! catalogue, because this crate never recommends a tool that is missing. The
//! built-in list is Rust data in [`BUILT_IN`], not Markdown files: it is empty until the features whose ops the first
//! recipes need exist, and a recipe is a few lines of template that is reviewed next to the ops it names. A host adds
//! its own with [`McpOptions::with_prompt`](crate::McpOptions::with_prompt).
//!
//! A template is plain text with two kinds of placeholder, both replaced in one pass (so an argument's value is never
//! read as another placeholder):
//!
//! - `{name}`: the value of the argument `name`, or nothing when an optional argument was left out. Braces that name
//!   no argument stay as written.
//! - `{tool:face.set_profile}`: the tool name of that op (`face_set_profile`), the form an agent calls it by.

use rmcp::ErrorData as McpError;
use rmcp::model::{GetPromptResult, JsonObject, Prompt, PromptArgument, PromptMessage, Role};
use serde_json::{Value, json};

use crate::catalog::Catalog;
use crate::names::tool_name;

/// One argument of a [`PromptDef`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PromptArg {
    /// The name a client sends, and the `{name}` placeholder in the template.
    pub name: &'static str,
    /// What to put here, shown to the user filling it in.
    pub description: &'static str,
    /// Whether `prompts/get` refuses a call that leaves it out.
    pub required: bool,
}

/// A recipe: a prompt the server offers when every op it needs exists.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PromptDef {
    /// The prompt's id, as a client lists and requests it (`make_face_oval`).
    pub name: &'static str,
    /// A short title for menus.
    pub title: &'static str,
    /// One sentence on what the recipe does.
    pub description: &'static str,
    /// The arguments it takes, in the order a client shows them.
    pub arguments: &'static [PromptArg],
    /// Op names (`face.set_profile`) the recipe's text relies on. The prompt is listed only when all of them are
    /// tools in the session; list every op the template names with `{tool:...}`.
    pub needs: &'static [&'static str],
    /// The text sent to the agent, with `{arg}` and `{tool:op.name}` placeholders (see the module docs).
    pub template: &'static str,
}

/// The recipes this crate ships (spec v3 §11). Each lists every op its text names in `needs`, so it is offered only to
/// a session whose tools can do what it says; the recipes whose ops don't exist yet (setting up a first stream, a
/// highlight as a short, tuning the life layer) are added with the features that bring those ops.
pub(crate) const BUILT_IN: &[PromptDef] = &[
    PromptDef {
        name: "create_character_from_sheet",
        title: "Create a character from this sheet",
        description: "Build a character from a character sheet: body, palette, face and hair, then check it.",
        arguments: &[
            PromptArg {
                name: "sheet",
                description: "The character sheet: a file path in the project, or a description of the character",
                required: true,
            },
            PromptArg { name: "name", description: "The new character's id (a lowercase slug)", required: false },
        ],
        needs: &["character.create", "body.set_template", "palette.set", "part.add", "face.set_profile"],
        template: "Create a character from this sheet: {sheet}\n\
            Work in this order, and group the edits into one transaction with {tool:tx.begin} and {tool:tx.commit} so the whole \
            build undoes as one step.\n\
            1. Read the sheet and write down what it shows: body type and proportions, skin, hair, eyes, clothes, colours.\n\
            2. {tool:character.create} for the character (id: {name}; choose a short slug from the sheet if none was given), \
               then {tool:body.set_template} if the sheet's body is not the default.\n\
            3. {tool:palette.set} for the colours you read off the sheet, one slot at a time.\n\
            4. {tool:part.add} for hair, garments and accessories, and {tool:face.set_profile} for the face.\n\
            5. Preview each edit with dry_run first. When the character is built, check it as the instructions say, and tell \
               me what you could not match from the sheet instead of guessing.",
    },
    PromptDef {
        name: "match_reference_look",
        title: "Match this reference look",
        description: "Bring a character's rendering style and colours close to a reference image or video.",
        arguments: &[
            PromptArg {
                name: "reference",
                description: "The reference: a file path in the project, or a description of the look",
                required: true,
            },
            PromptArg { name: "character", description: "The character to change", required: true },
        ],
        needs: &["look.list_presets", "look.set_preset", "look.set_param", "palette.set"],
        template: "Make {character} look like this reference: {reference}\n\
            1. Describe the reference's rendering style (outline weight, shading steps, bloom, texture) and its palette \
               before you touch anything, and measure the reference rather than judging it by eye.\n\
            2. {tool:look.list_presets}, then {tool:look.set_preset} for the closest preset.\n\
            3. {tool:look.set_param} for each remaining difference, one parameter at a time, previewing with dry_run.\n\
            4. {tool:palette.set} to match the reference's colours.\n\
            Group the edits in one transaction. Say which differences remain and why.",
    },
    PromptDef {
        name: "make_emote_pack",
        title: "Make an emote pack",
        description: "Create a set of named face expressions for a character.",
        arguments: &[
            PromptArg { name: "character", description: "The character the emotes are for", required: true },
            PromptArg {
                name: "emotes",
                description: "The emotes to make, for example: happy, sad, surprised, sleepy",
                required: false,
            },
        ],
        needs: &[
            "face.expression.set",
            "face.expression.duplicate",
            "face.expression.rename",
            "face.expression.values",
        ],
        template: "Make an emote pack for {character}. Emotes: {emotes} (if none were named, make happy, sad, angry, \
            surprised, sleepy and smug).\n\
            1. {tool:face.expression.values} to see which channels an expression can set.\n\
            2. {tool:face.expression.set} for each emote, starting from the closest one you have made with \
               {tool:face.expression.duplicate} and {tool:face.expression.rename}.\n\
            Keep every emote readable at small sizes: exaggerate eyes and mouth before anything else. Group the edits in one \
            transaction and list the emotes you made.",
    },
    PromptDef {
        name: "make_vrchat_quest_variant",
        title: "Make a VRChat Quest variant",
        description: "Create a lighter variant of a character for a mobile VRChat avatar, and check that it exports.",
        arguments: &[PromptArg {
            name: "character",
            description: "The character to make the variant of",
            required: true,
        }],
        needs: &["variant.create", "variant.set_override", "io.check_export"],
        template: "Make a Quest-sized variant of {character}.\n\
            1. {tool:variant.create} for a variant named quest that extends the base character.\n\
            2. {tool:variant.set_override} to turn off or simplify what a mobile avatar cannot afford (extra accessories, \
               secondary hair, heavy cloth). Change the variant only; the base character stays as it is.\n\
            3. {tool:io.check_export} for the VRM format, and fix what it reports until it passes or only things you must \
               ask me about remain.\n\
            Tell me what the variant gave up.",
    },
];

/// The prompts of one server: the built-in ones, then the host's.
#[derive(Clone, Debug)]
pub(crate) struct Prompts(Vec<PromptDef>);

impl Prompts {
    /// A host prompt with the name of an earlier one replaces it, so a host can override a built-in recipe.
    pub(crate) fn new(extra: &[PromptDef]) -> Self {
        let mut all: Vec<PromptDef> = Vec::with_capacity(BUILT_IN.len() + extra.len());
        for def in BUILT_IN.iter().chain(extra) {
            match all.iter_mut().find(|d| d.name == def.name) {
                Some(existing) => *existing = *def,
                None => all.push(*def),
            }
        }
        Self(all)
    }

    /// The prompts whose ops are all tools in `catalog`, sorted by name.
    fn available<'a>(&'a self, catalog: &'a Catalog) -> Vec<&'a PromptDef> {
        let mut found: Vec<&PromptDef> =
            self.0.iter().filter(|def| def.needs.iter().all(|op| catalog.get(op).is_some())).collect();
        found.sort_by_key(|def| def.name);
        found
    }

    /// What `prompts/list` returns for a session with this `catalog`.
    pub(crate) fn list(&self, catalog: &Catalog) -> Vec<Prompt> {
        self.available(catalog).into_iter().map(listing).collect()
    }

    /// What `prompts/get` returns: the named prompt with `arguments` filled in.
    ///
    /// # Errors
    /// `invalid_params` for a prompt this session isn't offered (naming those it is), an argument the prompt doesn't
    /// take or that isn't text, or a required argument that is missing.
    pub(crate) fn get(
        &self,
        catalog: &Catalog,
        name: &str,
        arguments: Option<JsonObject>,
    ) -> Result<GetPromptResult, McpError> {
        let available = self.available(catalog);
        let Some(def) = available.iter().find(|def| def.name == name) else {
            let names: Vec<&str> = available.iter().map(|def| def.name).collect();
            // The error rmcp's own prompt router gives for an unknown name.
            return Err(McpError::invalid_params(
                format!("prompt '{name}' not found"),
                Some(json!({"available_prompts": names})),
            ));
        };
        let text = fill(def, &arguments.unwrap_or_default())?;
        Ok(GetPromptResult::new(vec![PromptMessage::new_text(Role::User, text)]).with_description(def.description))
    }
}

fn listing(def: &PromptDef) -> Prompt {
    let arguments = def
        .arguments
        .iter()
        .map(|a| PromptArgument::new(a.name).with_description(a.description).with_required(a.required))
        .collect::<Vec<_>>();
    Prompt::new(def.name, Some(def.description), (!arguments.is_empty()).then_some(arguments)).with_title(def.title)
}

/// The template with its placeholders replaced, after checking the arguments against the definition.
fn fill(def: &PromptDef, given: &JsonObject) -> Result<String, McpError> {
    let known = || def.arguments.iter().map(|a| a.name).collect::<Vec<_>>();
    for (key, value) in given {
        if !def.arguments.iter().any(|a| a.name == key) {
            return Err(bad_argument(def, format!("{} doesn't take an argument `{key}`", def.name), &known()));
        }
        if !value.is_string() {
            return Err(bad_argument(def, format!("argument `{key}` must be text, got {value}"), &known()));
        }
    }
    if let Some(missing) = def.arguments.iter().find(|a| a.required && !given.contains_key(a.name)) {
        return Err(bad_argument(def, format!("{} needs the argument `{}`", def.name, missing.name), &known()));
    }
    let value_of = |name: &str| given.get(name).and_then(Value::as_str).unwrap_or_default();

    let mut out = String::with_capacity(def.template.len());
    let mut rest = def.template;
    while let Some(open) = rest.find('{') {
        out.push_str(&rest[..open]);
        rest = &rest[open..];
        let replacement = rest.find('}').and_then(|close| {
            let inner = &rest[1..close];
            let text = match inner.strip_prefix("tool:") {
                Some(op) => tool_name(op),
                None if def.arguments.iter().any(|a| a.name == inner) => value_of(inner).to_owned(),
                None => return None,
            };
            Some((text, close + 1))
        });
        if let Some((text, used)) = replacement {
            out.push_str(&text);
            rest = &rest[used..];
        } else {
            out.push('{');
            rest = &rest[1..];
        }
    }
    out.push_str(rest);
    Ok(out)
}

fn bad_argument(def: &PromptDef, message: String, known: &[&str]) -> McpError {
    McpError::invalid_params(message, Some(json!({"prompt": def.name, "arguments": known})))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ARGS: &[PromptArg] = &[
        PromptArg { name: "character", description: "Which character.", required: true },
        PromptArg { name: "note", description: "Extra wishes.", required: false },
    ];

    fn def(template: &'static str) -> PromptDef {
        PromptDef { name: "t", title: "T", description: "d", arguments: ARGS, needs: &[], template }
    }

    fn args(pairs: &[(&str, &str)]) -> JsonObject {
        pairs.iter().map(|(k, v)| ((*k).to_owned(), json!(v))).collect()
    }

    #[test]
    fn placeholders_are_replaced_in_one_pass() {
        let text =
            fill(&def("Edit {character} with {tool:face.set_profile}.{note}"), &args(&[("character", "{note}")]));
        assert_eq!(text.unwrap(), "Edit {note} with face_set_profile.");
    }

    #[test]
    fn braces_that_name_nothing_stay() {
        let text = fill(&def("json {\"a\": 1} {nope} { {character}"), &args(&[("character", "x")])).unwrap();
        assert_eq!(text, "json {\"a\": 1} {nope} { x");
    }

    #[test]
    fn a_missing_required_argument_is_named() {
        let error = fill(&def("{character}"), &JsonObject::new()).unwrap_err();
        assert!(error.message.contains("`character`"), "{}", error.message);
    }

    #[test]
    fn unknown_and_non_text_arguments_are_refused() {
        assert!(fill(&def("x"), &args(&[("character", "a"), ("typo", "b")])).is_err());
        let mut numeric = JsonObject::new();
        numeric.insert("character".to_owned(), json!(3));
        assert!(fill(&def("x"), &numeric).is_err());
    }

    #[test]
    fn a_later_prompt_with_the_same_name_replaces_the_earlier() {
        let (a, mut b) = (def("one"), def("two"));
        b.title = "B";
        let prompts = Prompts::new(&[a, b]);
        let named: Vec<_> = prompts.0.iter().filter(|d| d.name == "t").collect();
        assert_eq!(named.len(), 1);
        assert_eq!(named[0].template, "two");
        assert_eq!(prompts.0.len(), BUILT_IN.len() + 1, "the built-in recipes are kept");
    }

    /// The tool names a template names with `{tool:..}`.
    fn named_tools(template: &str) -> Vec<&str> {
        template.split("{tool:").skip(1).filter_map(|rest| rest.split('}').next()).collect()
    }

    #[test]
    fn every_tool_a_built_in_recipe_names_is_one_it_needs() {
        for def in BUILT_IN {
            for op in named_tools(def.template) {
                let built_in_tool = op.starts_with("tx.") || op.starts_with("history.");
                assert!(built_in_tool || def.needs.contains(&op), "{} names {op} without needing it", def.name);
            }
            for op in def.needs {
                assert!(named_tools(def.template).contains(op), "{} needs {op} but never names it", def.name);
            }
        }
    }

    #[test]
    fn every_placeholder_in_a_built_in_recipe_names_one_of_its_arguments() {
        for def in BUILT_IN {
            let mut rest = def.template;
            while let Some(open) = rest.find('{') {
                let close = rest[open..].find('}').map_or(rest.len(), |c| open + c);
                let inner = &rest[open + 1..close];
                assert!(
                    inner.starts_with("tool:") || def.arguments.iter().any(|a| a.name == inner),
                    "{}: {{{inner}}} names no argument",
                    def.name
                );
                rest = &rest[close.min(rest.len() - 1) + 1..];
            }
        }
    }

    #[test]
    fn built_in_recipe_names_are_unique_and_safe() {
        let mut names: Vec<&str> = BUILT_IN.iter().map(|d| d.name).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(names.len(), BUILT_IN.len());
        assert!(names.iter().all(|n| n.chars().all(|c| c.is_ascii_lowercase() || c == '_')));
    }
}
