//! The recipe `create_character_from_sheet` (spec v3 §11): offered to a session only when every op it needs is one of its tools.

use crate::prompts::{PromptArg, PromptDef};

const RECIPE: PromptDef = PromptDef {
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
};

register_recipe!(RECIPE);
