//! The one error type of the stream contract.

/// Why a stream operation failed.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum StreamError {
    /// A needed encoder, library or device is missing; `hint` tells a person what to do about it.
    #[error("{what} is unavailable: {hint}")]
    Unavailable {
        /// What is missing, e.g. `FFmpeg` libraries.
        what: String,
        /// An action a person can take, e.g. "install `FFmpeg` 7 or set `OMUZAKI_FFMPEG_DIR`".
        hint: String,
    },
    /// The configuration is invalid (a zero denominator, an unsupported size).
    #[error("invalid configuration: {0}")]
    Config(String),
    /// A read or write failed.
    #[error("i/o error: {0}")]
    Io(String),
    /// The encoder failed.
    #[error("encode error: {0}")]
    Encode(String),
    /// The muxer failed.
    #[error("mux error: {0}")]
    Mux(String),
    /// A bounded queue or byte cap is full; the caller drops or retries, it is never a silent loss.
    #[error("queue full")]
    Full,
    /// The resource is in use by another job (a save in progress).
    #[error("busy")]
    Busy,
    /// The output or encoder was stopped.
    #[error("stopped")]
    Stopped,
}
