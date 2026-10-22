//! [`TwitchError`]: the ways connecting to Twitch fails, each with text that says what to do. None carries the token.

/// Why Twitch couldn't be reached or used.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum TwitchError {
    /// The token is missing, refused or lacks what is needed.
    #[error("{0}")]
    Token(String),
    /// Twitch couldn't be reached.
    #[error("couldn't reach Twitch: {0}")]
    Network(String),
    /// Twitch answered something this adapter doesn't understand.
    #[error("Twitch answered unexpectedly: {0}")]
    Protocol(String),
    /// Twitch refused a subscription.
    #[error("Twitch refused the {kind} subscription: {reason}")]
    Refused {
        /// The subscription type.
        kind: String,
        /// Twitch's reason.
        reason: String,
    },
}
