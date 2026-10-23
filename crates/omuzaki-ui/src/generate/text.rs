//! The words of generation: the Generators page, the key field, the Generated tray and the cost card, each a named
//! constant whose id the lexicon makes from its name (`ui-generate-accept` for `ACCEPT`), said through
//! [`say`](crate::panels::text::say).

crate::ds::strings::lines! {
    prefix = "ui-generate";

    /// The Connectors page's segment for generators.
    pub const GENERATORS: &str = "Generators";
    /// No generator in this build.
    pub const NO_GENERATORS: &str = "No generators in this build";
    /// Why.
    pub const NO_GENERATORS_HOW: &str = "Generators are the image, texture, voice, music and model services Omuzaki can call. None is built into this copy.";
    /// The generators can't be read.
    pub const UNAVAILABLE: &str = "Generators aren't available";
    /// Why, from the op.
    pub const UNAVAILABLE_WHY: &str = "{reason}";
    /// The list's heading.
    pub const LIST: &str = "Generators";
    /// The screen-reader name of a generator's row.
    pub const ROW_NAME: &str = "{name}, {status}";

    /// Ready to run.
    pub const READY: &str = "Ready";
    /// Turned off.
    pub const DISABLED: &str = "Off";
    /// Needs its key.
    pub const NEEDS_KEY: &str = "Needs key";
    /// Needs another plan tier.
    pub const NEEDS_PLAN: &str = "Needs plan";
    /// Settings incomplete.
    pub const NOT_SET_UP: &str = "Not set up";
    /// Costs money.
    pub const PAID: &str = "Paid";
    /// Free.
    pub const FREE: &str = "Free";
    /// Data leaves the computer.
    pub const SENDS_TO: &str = "Sends to {host}";
    /// Nothing leaves.
    pub const STAYS: &str = "Stays on this computer";

    /// Images.
    pub const CAP_IMAGE: &str = "Images";
    /// Textures.
    pub const CAP_TEXTURE: &str = "Textures";
    /// 3D models.
    pub const CAP_MODEL3D: &str = "3D models";
    /// Speech.
    pub const CAP_VOICE: &str = "Voice";
    /// Music and sound.
    pub const CAP_MUSIC: &str = "Music";
    /// Motion.
    pub const CAP_MOTION: &str = "Motion";
    /// Text.
    pub const CAP_LLM: &str = "Text";
    /// Answers about images.
    pub const CAP_VISION: &str = "Vision";
    /// A program call.
    pub const CAP_DCC: &str = "Programs";

    /// The detail's privacy heading.
    pub const PRIVACY: &str = "What it sends";
    /// What a call sends.
    pub const SENDS: &str = "A call sends: {what}";
    /// The prompt.
    pub const SENDS_PROMPT: &str = "the prompt";
    /// Images.
    pub const SENDS_IMAGES: &str = "images";
    /// Audio.
    pub const SENDS_AUDIO: &str = "audio";
    /// Meshes.
    pub const SENDS_MESHES: &str = "meshes";
    /// Video.
    pub const SENDS_VIDEO: &str = "video";
    /// Other files.
    pub const SENDS_FILES: &str = "files";
    /// Nothing is sent.
    pub const SENDS_NOTHING: &str = "A call sends nothing off this computer.";
    /// Where it goes.
    pub const GOES_TO: &str = "It goes to: {hosts}";
    /// The vendor's retention words.
    pub const RETENTION: &str = "What they keep: {words}";
    /// Training on inputs: yes.
    pub const TRAINS_YES: &str = "They may train on what you send.";
    /// Training on inputs: no.
    pub const TRAINS_NO: &str = "They don't train on what you send.";
    /// Training on inputs: not said.
    pub const TRAINS_UNKNOWN: &str = "Their terms don't say whether they train on what you send.";
    /// The licence heading.
    pub const LICENCE: &str = "What you can do with the results";
    /// The results' licence.
    pub const LICENCE_LINE: &str = "Results carry the licence {licence}.";
    /// Commercial use allowed.
    pub const COMMERCIAL_OK: &str = "You can publish and sell them.";
    /// Commercial use not allowed.
    pub const COMMERCIAL_NO: &str = "Not for commercial use.";
    /// Commercial use not established.
    pub const COMMERCIAL_UNKNOWN: &str = "Whether you can sell them isn't known, so Omuzaki treats them as not for publishing.";
    /// The licence comes from the person's model.
    pub const LICENCE_FROM_MODEL: &str = "The licence is the one you declare for each model it runs.";
    /// A plan rule.
    pub const PLAN_RULE: &str = "Only these plans make publishable results: {plans}.";

    /// The enabled toggle.
    pub const ENABLED: &str = "Enabled";
    /// The endpoint field.
    pub const ENDPOINT: &str = "Address";
    /// The endpoint's hint.
    pub const ENDPOINT_HINT: &str = "http://127.0.0.1:8188";
    /// Save the endpoint.
    pub const ENDPOINT_SAVE: &str = "Save address";
    /// Use the manifest's endpoint.
    pub const ENDPOINT_RESET: &str = "Use the default";
    /// An endpoint off this computer.
    pub const ENDPOINT_LEAVES: &str = "This address is on another computer: what you send now leaves this one.";
    /// An endpoint on this computer.
    pub const ENDPOINT_LOCAL: &str = "This address is on this computer: nothing leaves it.";
    /// The plan dropdown.
    pub const PLAN: &str = "Your plan";
    /// No plan chosen.
    pub const PLAN_NONE: &str = "Not set";
    /// The declared model licences.
    pub const MODELS: &str = "Model licences you declared";
    /// One declared model.
    pub const MODEL_LINE: &str = "{model}: {licence}, {commercial}";
    /// Commercial yes, for a declared model.
    pub const MODEL_COMMERCIAL: &str = "commercial use allowed";
    /// Commercial no.
    pub const MODEL_NONCOMMERCIAL: &str = "not for commercial use";
    /// Commercial unknown.
    pub const MODEL_UNKNOWN: &str = "commercial use unknown";
    /// No model declared.
    pub const NO_MODELS: &str = "No model licences declared. Results are not publishable until you declare the model's licence.";
    /// Test the connection.
    pub const TEST: &str = "Test connection";
    /// Testing.
    pub const TESTING: &str = "Testing...";
    /// The test passed.
    pub const TEST_OK: &str = "It answered.";
    /// The test failed.
    pub const TEST_FAILED: &str = "It didn't answer: {reason}";
    /// A change refused.
    pub const NOT_SAVED: &str = "Not saved: {reason}";

    /// The key field.
    pub const KEY: &str = "API key";
    /// The key field's hint.
    pub const KEY_HINT: &str = "Paste the key";
    /// Save the key.
    pub const KEY_SAVE: &str = "Save key";
    /// Remove the key.
    pub const KEY_REMOVE: &str = "Remove key";
    /// Nothing typed.
    pub const KEY_EMPTY: &str = "Type or paste a key first";
    /// A key is stored.
    pub const KEY_SAVED: &str = "Key saved";
    /// No key stored.
    pub const KEY_MISSING: &str = "No key yet";
    /// A key too short.
    pub const KEY_SHORT: &str = "That doesn't look like a key";
    /// Where the key lives.
    pub const KEY_WHERE: &str = "The key goes to this computer's keychain. Omuzaki never shows it again, logs it or puts it in a project.";
    /// No keychain in this build.
    pub const NO_KEYCHAIN: &str = "This studio has no keychain to keep the key in.";
    /// The keychain refused.
    pub const KEY_FAILED: &str = "The key wasn't saved: {reason}";

    /// The tray panel's empty state.
    pub const TRAY_EMPTY: &str = "Nothing generated yet";
    /// How things arrive.
    pub const TRAY_EMPTY_HOW: &str = "What a generator makes lands here first. Accept it to use it, or reject it.";
    /// No tray in this build.
    pub const NO_TRAY: &str = "Generation isn't available";
    /// Why.
    pub const NO_TRAY_HOW: &str = "This studio was started without a store for generated files.";
    /// The filter.
    pub const SHOW: &str = "Show";
    /// Waiting for a decision.
    pub const PENDING: &str = "To decide";
    /// Accepted.
    pub const ACCEPTED: &str = "Accepted";
    /// Rejected.
    pub const REJECTED: &str = "Rejected";
    /// Every candidate.
    pub const ALL_STATES: &str = "All";
    /// A tile's name.
    pub const TILE_NAME: &str = "{prompt}, from {connector}, {id}";
    /// A tile with no prompt.
    pub const NO_PROMPT: &str = "Untitled";
    /// Accept.
    pub const ACCEPT: &str = "Accept";
    /// Reject.
    pub const REJECT: &str = "Reject";
    /// The reason field.
    pub const REJECT_WHY: &str = "Why? (optional)";
    /// Confirm rejecting.
    pub const REJECT_CONFIRM: &str = "Reject it";
    /// Make another like it.
    pub const REGENERATE: &str = "Make another";
    /// Compare two.
    pub const COMPARE: &str = "Compare";
    /// Stop comparing.
    pub const COMPARE_CLOSE: &str = "Close comparison";
    /// How to compare.
    pub const COMPARE_HOW: &str = "Choose two to compare them side by side.";
    /// Accept the chosen.
    pub const ACCEPT_CHOSEN: &str = "Accept {count}";
    /// Reject the chosen.
    pub const REJECT_CHOSEN: &str = "Reject {count}";
    /// The rights badge: publishable.
    pub const CAN_PUBLISH: &str = "Can be published";
    /// The rights badge: not publishable.
    pub const CANT_PUBLISH: &str = "Can't be published: {why}";
    /// Why: the licence is unknown.
    pub const WHY_UNKNOWN: &str = "unknown licence";
    /// Why: not commercial.
    pub const WHY_NONCOMMERCIAL: &str = "not for commercial use";
    /// Why: private test material.
    pub const WHY_PRIVATE: &str = "private test material";
    /// The licence on a tile.
    pub const TILE_LICENCE: &str = "Licence: {licence}";
    /// The model chip.
    pub const MODEL: &str = "{model}";
    /// The plan chip.
    pub const PLAN_CHIP: &str = "Plan {plan}";
    /// The cost chip.
    pub const COST: &str = "{usd}";
    /// A kind with no picture.
    pub const KIND_AUDIO: &str = "Audio";
    /// A mesh.
    pub const KIND_MESH: &str = "3D model";
    /// Motion.
    pub const KIND_MOTION: &str = "Motion";
    /// Video.
    pub const KIND_VIDEO: &str = "Video";
    /// Text.
    pub const KIND_TEXT: &str = "Text";
    /// The rejected reason.
    pub const REJECTED_BECAUSE: &str = "Rejected: {reason}";
    /// A change refused.
    pub const REFUSED: &str = "Not done: {reason}";

    /// The make form's generator.
    pub const MAKE_WITH: &str = "Generator";
    /// The prompt.
    pub const PROMPT: &str = "Describe it";
    /// How many.
    pub const COUNT: &str = "How many";
    /// Make.
    pub const MAKE: &str = "Generate";
    /// No ready generator.
    pub const MAKE_NONE: &str = "Turn on a generator in Settings, Connectors, Generators to make images here.";
    /// Open the settings.
    pub const OPEN_SETTINGS: &str = "Open Connectors";
    /// A run going on.
    pub const MAKING: &str = "Generating with {name}...";

    /// The cost card's title.
    pub const CARD_TITLE: &str = "Before it runs";
    /// What leaves and the price.
    pub const CARD_SENDS_PAID: &str = "This sends {what} to {hosts} and costs {about}{usd}.";
    /// What leaves, free.
    pub const CARD_SENDS_FREE: &str = "This sends {what} to {hosts}. It costs nothing.";
    /// Stays here, but costs.
    pub const CARD_PAID_LOCAL: &str = "This stays on this computer and costs {about}{usd}.";
    /// "about", for an upper bound.
    pub const ABOUT: &str = "up to ";
    /// A count of items sent.
    pub const COUNTED: &str = "{count} {what}";
    /// Joining what is sent.
    pub const AND: &str = " and ";
    /// The plan's licence line on the card.
    pub const CARD_LICENCE: &str = "Results: {licence}. {commercial}";
    /// The budget left.
    pub const CARD_BUDGET: &str = "Budget left this session: {usd}";
    /// Run it.
    pub const RUN: &str = "Run";
    /// Cancel.
    pub const CANCEL: &str = "Cancel";
    /// A free local run.
    pub const LOCAL_LINE: &str = "Stays on this computer";
    /// The price changed.
    pub const PRICE_CHANGED: &str = "The price changed before it ran. Check the new one.";
    /// Refused: a private input.
    pub const REFUSED_PRIVATE: &str = "It would send private test material, which never leaves this computer.";
    /// Refused: needs a key.
    pub const REFUSED_KEY: &str = "This generator needs its key first.";
    /// Refused: needs a plan.
    pub const REFUSED_PLAN: &str = "Set your plan for this generator first.";
    /// Refused: needs consent.
    pub const REFUSED_CONSENT: &str = "It would use a real person's likeness or voice without consent on record.";
    /// Refused, another reason.
    pub const REFUSED_OTHER: &str = "It can't run: {reason}";
    /// The run failed.
    pub const RUN_FAILED: &str = "It didn't run: {reason}";
    /// The run made files.
    pub const RUN_DONE: &str = "Made {count}: they're in Generated";
}
