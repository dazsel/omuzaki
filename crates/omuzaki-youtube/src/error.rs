//! What can go wrong talking to YouTube. No variant carries the token.

/// A YouTube failure.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum YouTubeError {
    /// YouTube couldn't be reached, or the call timed out.
    #[error("YouTube couldn't be reached: {0}")]
    Network(String),
    /// The token was refused: only a new one fixes it.
    #[error("{0}; store a new token under youtube/token")]
    Token(String),
    /// The project's daily quota is spent.
    #[error("the YouTube API quota for today is spent; it resets at midnight Pacific time")]
    Quota,
    /// Nothing is live on the channel (or the named video has no live chat).
    #[error("nothing is live: start the stream on YouTube first, or name the live video")]
    NotLive,
    /// The live chat ended (the stream went offline, or chat was turned off).
    #[error("the stream's live chat has ended")]
    Ended,
    /// YouTube refused the call, with its reason.
    #[error("YouTube refused: {0}")]
    Refused(String),
    /// YouTube answered something unexpected.
    #[error("{0}")]
    Protocol(String),
}
