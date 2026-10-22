//! The API limits as constants, and the validated newtypes that enforce them before a message is built.
//!
//! "README" below is the `DenchiSoft/VTubeStudio` README (see the crate docs for the version read).

use serde::{Deserialize, Serialize};

/// Most custom parameters one plugin may create (README, "Adding new tracking parameters").
pub const MAX_PLUGIN_PARAMS: usize = 100;
/// Hotkeys VTS queues before it answers with a queue-full error (README, "Requesting execution of hotkeys").
pub const HOTKEY_QUEUE: usize = 32;
/// Frames between two triggers of one hotkey (README, "Requesting execution of hotkeys").
pub const HOTKEY_MIN_FRAMES: u32 = 5;
/// A parameter we control must be re-sent at least this often, or VTS drops it back to its previous controller
/// (README, "Feeding in data for default or custom parameters").
pub const KEEPALIVE: std::time::Duration = std::time::Duration::from_secs(1);
/// The default WebSocket port of the VTS API (README, "API Details"); the user can change it in VTS.
pub const DEFAULT_PORT: u16 = 8001;
/// The UDP port VTS broadcasts its presence on (README, "API Server Discovery (UDP)").
pub const DISCOVERY_PORT: u16 = 47779;
/// The shortest custom parameter name (README, "Adding new tracking parameters").
pub const PARAM_NAME_MIN: usize = 4;
/// The longest custom parameter name (README, "Adding new tracking parameters").
pub const PARAM_NAME_MAX: usize = 32;
/// The largest magnitude of a parameter value (README, "Feeding in data for default or custom parameters").
pub const PARAM_VALUE_LIMIT: f64 = 1_000_000.0;
/// The longest expression fade (README, "Requesting activation or deactivation of expressions").
pub const FADE_TIME_MAX: f64 = 2.0;
/// The fade used when none is given (README, "Requesting activation or deactivation of expressions").
pub const FADE_TIME_DEFAULT: f64 = 0.25;

/// A value that breaks a documented limit.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum LimitError {
    /// The name is not 4 to 32 ASCII letters and digits.
    #[error("custom parameter name {0:?} must be {PARAM_NAME_MIN} to {PARAM_NAME_MAX} ASCII letters or digits")]
    ParamName(String),
    /// The value is NaN, infinite or beyond ±1,000,000.
    #[error("parameter value {0} must be finite and within ±{PARAM_VALUE_LIMIT}")]
    ParamValue(f64),
}

/// A custom parameter name: ASCII alphanumeric, 4 to 32 characters.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct CustomParamName(String);

impl CustomParamName {
    /// Checks `name` against the documented rules.
    ///
    /// # Errors
    /// [`LimitError::ParamName`] when it is too short, too long or has a character that is not an ASCII letter or digit.
    pub fn new(name: impl Into<String>) -> Result<Self, LimitError> {
        let name = name.into();
        let ok =
            (PARAM_NAME_MIN..=PARAM_NAME_MAX).contains(&name.len()) && name.bytes().all(|b| b.is_ascii_alphanumeric());
        if ok { Ok(Self(name)) } else { Err(LimitError::ParamName(name)) }
    }

    /// The name as sent on the wire.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for CustomParamName {
    type Error = LimitError;
    fn try_from(name: String) -> Result<Self, LimitError> {
        Self::new(name)
    }
}

impl From<CustomParamName> for String {
    fn from(name: CustomParamName) -> Self {
        name.0
    }
}

/// A parameter value VTS accepts: finite and within ±1,000,000.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "f64", into = "f64")]
pub struct ParamValue(f64);

impl ParamValue {
    /// Checks `value` against the documented range.
    ///
    /// # Errors
    /// [`LimitError::ParamValue`] for NaN, infinities and anything beyond ±1,000,000.
    pub fn new(value: f64) -> Result<Self, LimitError> {
        if value.is_finite() && value.abs() <= PARAM_VALUE_LIMIT {
            Ok(Self(value))
        } else {
            Err(LimitError::ParamValue(value))
        }
    }

    /// The value.
    pub fn get(self) -> f64 {
        self.0
    }

    /// `value` clamped into the range, and 0 for NaN: for values computed every frame, where a refusal helps nobody.
    #[must_use]
    pub fn saturating(value: f64) -> Self {
        if value.is_nan() { Self(0.0) } else { Self(value.clamp(-PARAM_VALUE_LIMIT, PARAM_VALUE_LIMIT)) }
    }
}

impl TryFrom<f64> for ParamValue {
    type Error = LimitError;
    fn try_from(value: f64) -> Result<Self, LimitError> {
        Self::new(value)
    }
}

impl From<ParamValue> for f64 {
    fn from(value: ParamValue) -> Self {
        value.0
    }
}

/// An expression fade time in seconds, clamped to 0..=2 (VTS clamps the same way; we do it first so what we log is what runs).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(from = "f64", into = "f64")]
pub struct FadeTime(f64);

impl FadeTime {
    /// Clamps `seconds` to 0..=2. A NaN or infinite input gives the default of 0.25 s.
    pub fn new(seconds: f64) -> Self {
        if seconds.is_finite() { Self(seconds.clamp(0.0, FADE_TIME_MAX)) } else { Self::default() }
    }

    /// The fade in seconds.
    pub fn seconds(self) -> f64 {
        self.0
    }
}

impl Default for FadeTime {
    fn default() -> Self {
        Self(FADE_TIME_DEFAULT)
    }
}

impl From<f64> for FadeTime {
    fn from(seconds: f64) -> Self {
        Self::new(seconds)
    }
}

impl From<FadeTime> for f64 {
    fn from(fade: FadeTime) -> Self {
        fade.0
    }
}
