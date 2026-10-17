//! The recipe `make_vrchat_quest_variant` (spec v3 §11): offered to a session only when every op it needs is one of its tools.

use crate::prompts::{PromptArg, PromptDef};

const RECIPE: PromptDef = PromptDef {
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
};

register_recipe!(RECIPE);
