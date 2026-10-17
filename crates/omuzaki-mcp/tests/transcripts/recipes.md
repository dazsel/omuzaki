# the built-in recipes

`prompts/list` offers the four recipes whose ops exist; each `prompts/get` fills in its arguments and names the tools by the names the agent calls; a missing required argument is `invalid_params`.

Each block is one JSON-RPC message as it crossed an in-memory stream.

`-->`
```json
{
  "id": 1,
  "jsonrpc": "2.0",
  "method": "initialize",
  "params": {
    "capabilities": {},
    "clientInfo": {
      "name": "claude-code",
      "version": "1.0"
    },
    "protocolVersion": "2025-11-25"
  }
}
```

`<--`
```json
{
  "id": 1,
  "jsonrpc": "2.0",
  "result": {
    "capabilities": {
      "prompts": {},
      "resources": {
        "subscribe": true
      },
      "tools": {}
    },
    "instructions": "Omuzaki is a character studio. Every tool is an operation on the project; the tool list is generated from the op registry.\nWorking method:\n- Look before you edi [...the rest of the server guide...]",
    "protocolVersion": "2025-11-25",
    "serverInfo": {
      "name": "omuzaki",
      "title": "Omuzaki Studio",
      "version": "0.0.0"
    }
  }
}
```

`-->`
```json
{
  "jsonrpc": "2.0",
  "method": "notifications/initialized",
  "params": {}
}
```

`-->`
```json
{
  "id": 2,
  "jsonrpc": "2.0",
  "method": "prompts/list",
  "params": {}
}
```

`<--`
```json
{
  "id": 2,
  "jsonrpc": "2.0",
  "result": {
    "prompts": [
      {
        "arguments": [
          {
            "description": "The character sheet: a file path in the project, or a description of the character",
            "name": "sheet",
            "required": true
          },
          {
            "description": "The new character's id (a lowercase slug)",
            "name": "name",
            "required": false
          }
        ],
        "description": "Build a character from a character sheet: body, palette, face and hair, then check it.",
        "name": "create_character_from_sheet",
        "title": "Create a character from this sheet"
      },
      {
        "arguments": [
          {
            "description": "The character the emotes are for",
            "name": "character",
            "required": true
          },
          {
            "description": "The emotes to make, for example: happy, sad, surprised, sleepy",
            "name": "emotes",
            "required": false
          }
        ],
        "description": "Create a set of named face expressions for a character.",
        "name": "make_emote_pack",
        "title": "Make an emote pack"
      },
      {
        "arguments": [
          {
            "description": "The character to make the variant of",
            "name": "character",
            "required": true
          }
        ],
        "description": "Create a lighter variant of a character for a mobile VRChat avatar, and check that it exports.",
        "name": "make_vrchat_quest_variant",
        "title": "Make a VRChat Quest variant"
      },
      {
        "arguments": [
          {
            "description": "The reference: a file path in the project, or a description of the look",
            "name": "reference",
            "required": true
          },
          {
            "description": "The character to change",
            "name": "character",
            "required": true
          }
        ],
        "description": "Bring a character's rendering style and colours close to a reference image or video.",
        "name": "match_reference_look",
        "title": "Match this reference look"
      }
    ]
  }
}
```

`-->`
```json
{
  "id": 3,
  "jsonrpc": "2.0",
  "method": "prompts/get",
  "params": {
    "arguments": {
      "name": "mira",
      "sheet": "sheets/mira.png"
    },
    "name": "create_character_from_sheet"
  }
}
```

`<--`
```json
{
  "id": 3,
  "jsonrpc": "2.0",
  "result": {
    "description": "Build a character from a character sheet: body, palette, face and hair, then check it.",
    "messages": [
      {
        "content": {
          "text": "Create a character from this sheet: sheets/mira.png\nWork in this order, and group the edits into one transaction with tx_begin and tx_commit so the whole build undoes as one step.\n1. Read the sheet and write down what it shows: body type and proportions, skin, hair, eyes, clothes, colours.\n2. character_create for the character (id: mira; choose a short slug from the sheet if none was given), then body_set_template if the sheet's body is not the default.\n3. palette_set for the colours you read off the sheet, one slot at a time.\n4. part_add for hair, garments and accessories, and face_set_profile for the face.\n5. Preview each edit with dry_run first. When the character is built, check it as the instructions say, and tell me what you could not match from the sheet instead of guessing.",
          "type": "text"
        },
        "role": "user"
      }
    ]
  }
}
```

`-->`
```json
{
  "id": 4,
  "jsonrpc": "2.0",
  "method": "prompts/get",
  "params": {
    "arguments": {
      "character": "mira",
      "reference": "refs/anime-key-art.png"
    },
    "name": "match_reference_look"
  }
}
```

`<--`
```json
{
  "id": 4,
  "jsonrpc": "2.0",
  "result": {
    "description": "Bring a character's rendering style and colours close to a reference image or video.",
    "messages": [
      {
        "content": {
          "text": "Make mira look like this reference: refs/anime-key-art.png\n1. Describe the reference's rendering style (outline weight, shading steps, bloom, texture) and its palette before you touch anything, and measure the reference rather than judging it by eye.\n2. look_list_presets, then look_set_preset for the closest preset.\n3. look_set_param for each remaining difference, one parameter at a time, previewing with dry_run.\n4. palette_set to match the reference's colours.\nGroup the edits in one transaction. Say which differences remain and why.",
          "type": "text"
        },
        "role": "user"
      }
    ]
  }
}
```

`-->`
```json
{
  "id": 5,
  "jsonrpc": "2.0",
  "method": "prompts/get",
  "params": {
    "arguments": {
      "character": "mira",
      "emotes": "happy, sad, surprised"
    },
    "name": "make_emote_pack"
  }
}
```

`<--`
```json
{
  "id": 5,
  "jsonrpc": "2.0",
  "result": {
    "description": "Create a set of named face expressions for a character.",
    "messages": [
      {
        "content": {
          "text": "Make an emote pack for mira. Emotes: happy, sad, surprised (if none were named, make happy, sad, angry, surprised, sleepy and smug).\n1. face_expression_values to see which channels an expression can set.\n2. face_expression_set for each emote, starting from the closest one you have made with face_expression_duplicate and face_expression_rename.\nKeep every emote readable at small sizes: exaggerate eyes and mouth before anything else. Group the edits in one transaction and list the emotes you made.",
          "type": "text"
        },
        "role": "user"
      }
    ]
  }
}
```

`-->`
```json
{
  "id": 6,
  "jsonrpc": "2.0",
  "method": "prompts/get",
  "params": {
    "arguments": {
      "character": "mira"
    },
    "name": "make_vrchat_quest_variant"
  }
}
```

`<--`
```json
{
  "id": 6,
  "jsonrpc": "2.0",
  "result": {
    "description": "Create a lighter variant of a character for a mobile VRChat avatar, and check that it exports.",
    "messages": [
      {
        "content": {
          "text": "Make a Quest-sized variant of mira.\n1. variant_create for a variant named quest that extends the base character.\n2. variant_set_override to turn off or simplify what a mobile avatar cannot afford (extra accessories, secondary hair, heavy cloth). Change the variant only; the base character stays as it is.\n3. io_check_export for the VRM format, and fix what it reports until it passes or only things you must ask me about remain.\nTell me what the variant gave up.",
          "type": "text"
        },
        "role": "user"
      }
    ]
  }
}
```

`-->`
```json
{
  "id": 7,
  "jsonrpc": "2.0",
  "method": "prompts/get",
  "params": {
    "arguments": {},
    "name": "make_emote_pack"
  }
}
```

`<--`
```json
{
  "error": {
    "code": -32602,
    "data": {
      "arguments": [
        "character",
        "emotes"
      ],
      "prompt": "make_emote_pack"
    },
    "message": "make_emote_pack needs the argument `character`"
  },
  "id": 7,
  "jsonrpc": "2.0"
}
```
