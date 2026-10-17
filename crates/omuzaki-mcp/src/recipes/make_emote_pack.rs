//! The recipe `make_emote_pack` (spec v3 §11): offered to a session only when every op it needs is one of its tools.

use crate::prompts::{PromptArg, PromptDef};

const RECIPE: PromptDef = PromptDef {
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
};

register_recipe!(RECIPE);
