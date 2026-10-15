//! What the person sends: [`UserMessage`] and the [`ContextChip`]s attached to it (D10).

use omuzaki_doc_api::DocPath;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Identifies a pending draw-to-direct note (#65). No such type exists elsewhere yet, so it lives here.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(transparent)]
pub struct NoteId(pub u64);

/// A thing the person attached to a message. Never a camera frame or captured audio: no chip can carry either (D6).
#[derive(Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub enum ContextChip {
    /// These document paths.
    Selection(Vec<DocPath>),
    /// The viewport as it is now (a render, never a camera frame).
    Viewport,
    /// A draw-to-direct note.
    Note(NoteId),
    /// A picture the person pasted or dropped.
    #[non_exhaustive]
    Image {
        /// The PNG bytes.
        png: Vec<u8>,
    },
    /// A resource URI, such as `omuzaki://character/main`.
    Resource(String),
}

impl ContextChip {
    /// A pasted image.
    pub fn image(png: Vec<u8>) -> Self {
        Self::Image { png }
    }
}

impl std::fmt::Debug for ContextChip {
    /// Image bytes are left out: a log line should not hold a picture.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Selection(paths) => write!(f, "Selection({} paths)", paths.len()),
            Self::Viewport => f.write_str("Viewport"),
            Self::Note(id) => write!(f, "Note({})", id.0),
            Self::Image { png } => write!(f, "Image({} bytes)", png.len()),
            Self::Resource(uri) => write!(f, "Resource({uri})"),
        }
    }
}

/// One message from the person: text plus the chips they attached.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct UserMessage {
    /// What they typed.
    pub text: String,
    /// What they attached.
    pub chips: Vec<ContextChip>,
}

impl UserMessage {
    /// A message of `text` with no chips.
    pub fn new(text: impl Into<String>) -> Self {
        Self { text: text.into(), chips: Vec::new() }
    }

    /// Attaches `chips`.
    #[must_use]
    pub fn with_chips(mut self, chips: Vec<ContextChip>) -> Self {
        self.chips = chips;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_chip_survives_json() {
        let path: DocPath = DocPath::parse("characters/main").unwrap();
        let chips = vec![
            ContextChip::Selection(vec![path]),
            ContextChip::Viewport,
            ContextChip::Note(NoteId(4)),
            ContextChip::image(vec![137, 80, 78, 71]),
            ContextChip::Resource("omuzaki://character/main".into()),
        ];
        let msg = UserMessage::new("make her face more oval").with_chips(chips);
        let json = serde_json::to_string(&msg).unwrap();
        assert_eq!(serde_json::from_str::<UserMessage>(&json).unwrap(), msg);
    }

    #[test]
    fn debug_leaves_image_bytes_out() {
        let shown = format!("{:?}", ContextChip::image(vec![0; 5000]));
        assert_eq!(shown, "Image(5000 bytes)");
    }
}
