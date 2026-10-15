//! The protocol's numbers: the default port, the RPC version, the event-subscription bits, the close codes and the
//! request status codes the bridge acts on. All come from the protocol document (see the crate docs).

/// The port the server listens on by default; the user can change it in OBS ("Tools", "WebSocket Server Settings").
pub const DEFAULT_PORT: u16 = 4455;
/// The RPC version this crate speaks (the document's "current RPC version"); sent in `Identify`.
pub const RPC_VERSION: u32 = 1;
/// The `requestStatus.code` of a request that succeeded.
pub const STATUS_SUCCESS: u32 = 100;
/// The `requestStatus.code` for a scene, item or other resource that does not exist.
pub const STATUS_RESOURCE_NOT_FOUND: u32 = 600;

/// The `eventSubscriptions` bitmask of `Identify` and `Reidentify`, and the `eventIntent` of an event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct EventSubscription(pub u32);

impl EventSubscription {
    /// No events.
    pub const NONE: Self = Self(0);
    /// General events (bit 0).
    pub const GENERAL: Self = Self(1 << 0);
    /// Configuration events (bit 1).
    pub const CONFIG: Self = Self(1 << 1);
    /// Scene events (bit 2).
    pub const SCENES: Self = Self(1 << 2);
    /// Input events (bit 3).
    pub const INPUTS: Self = Self(1 << 3);
    /// Transition events (bit 4).
    pub const TRANSITIONS: Self = Self(1 << 4);
    /// Filter events (bit 5).
    pub const FILTERS: Self = Self(1 << 5);
    /// Output events: stream, record, replay buffer (bit 6).
    pub const OUTPUTS: Self = Self(1 << 6);
    /// Scene item events (bit 7).
    pub const SCENE_ITEMS: Self = Self(1 << 7);
    /// Media input events (bit 8).
    pub const MEDIA_INPUTS: Self = Self(1 << 8);
    /// Vendor events (bit 9).
    pub const VENDORS: Self = Self(1 << 9);
    /// UI events (bit 10).
    pub const UI: Self = Self(1 << 10);
    /// Canvas events (bit 11); added after the first 5.x servers.
    pub const CANVASES: Self = Self(1 << 11);
    /// Every category up to and including [`UI`](Self::UI) (bits 0 to 10, 2047). Every 5.x server knows these bits, so
    /// this is the safe choice against an older OBS.
    pub const ALL_PRE_CANVASES: Self = Self((1 << 11) - 1);
    /// The document's `All`: every non-high-volume category, now including [`CANVASES`](Self::CANVASES) (bits 0 to 11,
    /// 4095).
    pub const ALL: Self = Self((1 << 12) - 1);
    /// Input volume meters (bit 16); high-volume, so not in [`ALL`](Self::ALL).
    pub const INPUT_VOLUME_METERS: Self = Self(1 << 16);
    /// Input active-state changes (bit 17); high-volume.
    pub const INPUT_ACTIVE_STATE_CHANGED: Self = Self(1 << 17);
    /// Input show-state changes (bit 18); high-volume.
    pub const INPUT_SHOW_STATE_CHANGED: Self = Self(1 << 18);
    /// Scene item transform changes (bit 19); high-volume.
    pub const SCENE_ITEM_TRANSFORM_CHANGED: Self = Self(1 << 19);

    /// Whether every bit of `other` is set.
    #[must_use]
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// The union of both sets.
    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }
}

/// The WebSocket close code the server uses to say why it ended the session. Numbers outside the documented table
/// are kept as [`Other`](Self::Other). The document's internal `DontClose` (0) is not a code that goes on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CloseCode {
    /// 4000: no more specific reason.
    UnknownReason,
    /// 4002: the message could not be decoded.
    MessageDecodeError,
    /// 4003: a required data field is missing.
    MissingDataField,
    /// 4004: a data field has the wrong type.
    InvalidDataFieldType,
    /// 4005: a data field has an invalid value.
    InvalidDataFieldValue,
    /// 4006: the `op` is not known.
    UnknownOpCode,
    /// 4007: the client sent something before `Identify`.
    NotIdentified,
    /// 4008: the client identified twice.
    AlreadyIdentified,
    /// 4009: the authentication string was wrong.
    AuthenticationFailed,
    /// 4010: the server does not support the RPC version asked for.
    UnsupportedRpcVersion,
    /// 4011: the session was invalidated, for example by the user kicking it.
    SessionInvalidated,
    /// 4012: the client used a feature this server lacks.
    UnsupportedFeature,
    /// Any other number.
    Other(u16),
}

impl CloseCode {
    /// The close code for a number off the wire.
    #[must_use]
    pub const fn from_u16(code: u16) -> Self {
        match code {
            4000 => Self::UnknownReason,
            4002 => Self::MessageDecodeError,
            4003 => Self::MissingDataField,
            4004 => Self::InvalidDataFieldType,
            4005 => Self::InvalidDataFieldValue,
            4006 => Self::UnknownOpCode,
            4007 => Self::NotIdentified,
            4008 => Self::AlreadyIdentified,
            4009 => Self::AuthenticationFailed,
            4010 => Self::UnsupportedRpcVersion,
            4011 => Self::SessionInvalidated,
            4012 => Self::UnsupportedFeature,
            other => Self::Other(other),
        }
    }

    /// The number on the wire.
    #[must_use]
    pub const fn as_u16(self) -> u16 {
        match self {
            Self::UnknownReason => 4000,
            Self::MessageDecodeError => 4002,
            Self::MissingDataField => 4003,
            Self::InvalidDataFieldType => 4004,
            Self::InvalidDataFieldValue => 4005,
            Self::UnknownOpCode => 4006,
            Self::NotIdentified => 4007,
            Self::AlreadyIdentified => 4008,
            Self::AuthenticationFailed => 4009,
            Self::UnsupportedRpcVersion => 4010,
            Self::SessionInvalidated => 4011,
            Self::UnsupportedFeature => 4012,
            Self::Other(code) => code,
        }
    }
}
