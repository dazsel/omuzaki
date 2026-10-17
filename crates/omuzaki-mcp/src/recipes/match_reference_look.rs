//! The recipe `match_reference_look` (spec v3 §11): offered to a session only when every op it needs is one of its tools.

use crate::prompts::{PromptArg, PromptDef};

const RECIPE: PromptDef = PromptDef {
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
};

register_recipe!(RECIPE);
