//! What both providers do the same way before and after a turn.

use std::sync::Arc;

use omuzaki_llm_api::error::LlmError;
use omuzaki_llm_api::message::{ContentBlock, ImageOrigin, TurnRequest};
use omuzaki_llm_api::provider::{ProviderConfig, ProviderEnv, SecretLabel, SecretStore};

/// The key's label and the store to resolve it in. The value is read per request and never kept (llm-api D6).
pub(crate) type KeyRef = Option<(SecretLabel, Arc<dyn SecretStore>)>;

/// The key reference of `config`, with the store it resolves in.
pub(crate) fn key_ref(config: &ProviderConfig, env: &ProviderEnv) -> KeyRef {
    config.key.as_ref().map(|key| (key.0.clone(), Arc::clone(&env.secrets)))
}

fn has_camera_image(blocks: &[ContentBlock]) -> bool {
    blocks.iter().any(|b| match b {
        ContentBlock::Image { origin, .. } => *origin == ImageOrigin::Camera,
        ContentBlock::ToolResult { blocks, .. } => has_camera_image(blocks),
        _ => false,
    })
}

/// A camera frame never reaches a client whose endpoint is not loopback (llm-api D7).
///
/// # Errors
/// `Unsupported` if `req` holds an image from the camera and the endpoint is remote.
pub(crate) fn refuse_camera(req: &TurnRequest, loopback: bool) -> Result<(), LlmError> {
    if !loopback && req.messages.iter().any(|m| has_camera_image(&m.content)) {
        return Err(LlmError::Unsupported("a camera image never leaves this machine: use a local model".into()));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use omuzaki_llm_api::message::{ImageMime, Message, Role};

    fn with(origin: ImageOrigin, nested: bool) -> TurnRequest {
        let image = ContentBlock::image(ImageMime::Png, vec![1], origin);
        let block = if nested { ContentBlock::tool_result("t", vec![image]) } else { image };
        TurnRequest::new(vec![Message::new(Role::User, vec![block])], 8)
    }

    #[test]
    fn a_camera_frame_is_refused_off_loopback_even_inside_a_tool_result() {
        for nested in [false, true] {
            assert!(matches!(refuse_camera(&with(ImageOrigin::Camera, nested), false), Err(LlmError::Unsupported(_))));
            assert_eq!(refuse_camera(&with(ImageOrigin::Camera, nested), true), Ok(()));
            assert_eq!(refuse_camera(&with(ImageOrigin::Screen, nested), false), Ok(()));
        }
    }
}
