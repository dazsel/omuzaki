//! The channel sweep (FE-04, FE-01): paint the face at rest and with each channel pushed to its far end, and measure how
//! much every layer changed. A channel whose push changes no layer by [`SweepOptions::min_changed_px`] is a **dead**
//! control: a tracker or an expression would drive it and the viewer would see nothing.
//!
//! The rest is the character's own resting buffer (`FaceProfile::rest_buf`), so a character that rests with a channel
//! already at its maximum is swept toward the minimum instead. Three channels have no painted meaning in a front-facing
//! 2D face, by contract (`jawForward`, `mouthShrugLower`, `mouthShrugUpper`): they are reported as **unmapped**, not
//! dead.
//!
//! A few channels only show in a setting ([`context`]): fangs need an open mouth, so they are pushed over a slightly open
//! jaw and measured against that. A channel the face's own design switches off (fangs on a face without them) is
//! reported as **off in the design**: not a broken control, a choice.

use image::RgbaImage;
use omuzaki_anim_api::{CHANNELS, Channel, Region};
use omuzaki_face_api::{FaceError, FaceLayerId};
use serde::Serialize;

use super::metrics::{LayerChange, layer_change};
use super::{FaceSubject, Preview};

/// The channels a front-facing painted face does not draw, by contract.
pub const UNMAPPED: [Channel; 3] = [Channel::JAW_FORWARD, Channel::MOUTH_SHRUG_LOWER, Channel::MOUTH_SHRUG_UPPER];

/// The setting a channel is pushed in, when it only shows in one: the channels set first, at rest and pushed alike.
pub fn context(channel: Channel) -> &'static [(Channel, f32)] {
    match channel {
        // Fangs hang from the upper lip of an open mouth (a closed one shows a fang only in a smile).
        Channel::FANGS => &[(Channel::JAW_OPEN, 0.35)],
        _ => &[],
    }
}

/// The width the dead-control threshold is stated at, and the threshold there (pixels changed in a layer).
pub const REFERENCE_WIDTH_PX: u32 = 1548;
/// See [`REFERENCE_WIDTH_PX`].
pub const REFERENCE_MIN_CHANGED_PX: u32 = 40;

/// The face channels: everything a face drives (brows, lids, gaze, mouth and the anime extras), in channel order. Head,
/// body and custom channels are not the face's.
pub fn face_channels() -> Vec<Channel> {
    CHANNELS
        .iter()
        .filter(|c| matches!(c.region, Region::Brows | Region::Eyes | Region::Gaze | Region::Mouth | Region::Extras))
        .map(|c| c.channel)
        .collect()
}

/// The dead-control threshold at `width_px`: [`REFERENCE_MIN_CHANGED_PX`] scaled by area from [`REFERENCE_WIDTH_PX`],
/// at least 1.
pub fn default_min_changed_px(width_px: u32) -> u32 {
    let ratio = f64::from(width_px) / f64::from(REFERENCE_WIDTH_PX);
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // a small positive pixel count
    let px = (f64::from(REFERENCE_MIN_CHANGED_PX) * ratio * ratio).round() as u32;
    px.max(1)
}

/// What to sweep and how strictly.
#[derive(Clone, Debug, PartialEq)]
pub struct SweepOptions {
    /// The channels to push, in the order they are reported.
    pub channels: Vec<Channel>,
    /// The picture width, pixels.
    pub width_px: u32,
    /// A channel is dead when no layer changes in more pixels than this.
    pub min_changed_px: u32,
    /// Keep the pushed picture of every channel (for a contact sheet).
    pub keep_pictures: bool,
}

/// How a channel did.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ChannelStatus {
    /// It visibly changes the face.
    Moves,
    /// It changes no layer by the threshold.
    Dead,
    /// The contract leaves it undrawn (see [`UNMAPPED`]).
    Unmapped,
    /// The face's design switches it off (fangs on a face without fangs).
    OffInDesign,
}

/// One channel's row.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ChannelSweep {
    /// The channel, by name.
    pub channel: &'static str,
    /// The value it was pushed to.
    pub value: f32,
    /// Moves, dead or unmapped.
    pub status: ChannelStatus,
    /// How each layer changed, in `FaceLayerId::ALL` order (`base`, `brows`, `overlay`).
    pub base: LayerChange,
    /// See [`base`](Self::base).
    pub brows: LayerChange,
    /// See [`base`](Self::base).
    pub overlay: LayerChange,
    /// The face with the channel pushed, when [`SweepOptions::keep_pictures`] asked for it.
    #[serde(skip)]
    pub picture: Option<RgbaImage>,
}

impl ChannelSweep {
    /// The largest changed-pixel count over the layers.
    pub fn most_changed_px(&self) -> u32 {
        self.base.changed_px.max(self.brows.changed_px).max(self.overlay.changed_px)
    }
}

/// The whole sweep.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct SweepReport {
    /// One row per swept channel, in the order asked.
    pub rows: Vec<ChannelSweep>,
    /// The face at rest, when pictures were kept.
    #[serde(skip)]
    pub rest: Option<RgbaImage>,
}

impl SweepReport {
    /// The names of the channels with `status`, in row order.
    pub fn with_status(&self, status: ChannelStatus) -> Vec<&'static str> {
        self.rows.iter().filter(|r| r.status == status).map(|r| r.channel).collect()
    }
}

/// Sweeps `options.channels` over `subject`. `cancelled` is polled between channels; `None` means it was cancelled.
///
/// # Errors
/// What the painter or the rasteriser returns.
pub fn sweep(
    subject: &FaceSubject,
    options: &SweepOptions,
    cancelled: &dyn Fn() -> bool,
) -> Result<Option<SweepReport>, FaceError> {
    let rest_buf = subject.profile.rest_buf();
    let mut rest = Preview::new(subject.clone(), options.width_px);
    rest.paint(&rest_buf)?;
    let rest_picture = options.keep_pictures.then(|| rest.picture());
    let mut pushed = Preview::new(subject.clone(), options.width_px);
    let mut rows = Vec::with_capacity(options.channels.len());
    // The setting the rest was last painted in, so the plain rest is repainted only after a channel with a context.
    let mut rest_context: &[(Channel, f32)] = &[];
    for &channel in &options.channels {
        if cancelled() {
            return Ok(None);
        }
        let setting = context(channel);
        let mut before = rest_buf;
        for &(c, v) in setting {
            before.set(c, v);
        }
        if setting != rest_context {
            rest.paint(&before)?;
            rest_context = setting;
        }
        let (lo, hi) = channel.range();
        let value = if before.get(channel) >= hi { lo } else { hi };
        let mut buf = before;
        buf.set(channel, value);
        pushed.paint(&buf)?;
        let change = |layer: FaceLayerId| layer_change(rest.layer(layer), pushed.layer(layer));
        let (base, brows, overlay) =
            (change(FaceLayerId::Base), change(FaceLayerId::Brows), change(FaceLayerId::Overlay));
        let most = base.changed_px.max(brows.changed_px).max(overlay.changed_px);
        let status = if UNMAPPED.contains(&channel) {
            ChannelStatus::Unmapped
        } else if channel == Channel::FANGS && !subject.profile.has_fangs() {
            ChannelStatus::OffInDesign
        } else if most <= options.min_changed_px {
            ChannelStatus::Dead
        } else {
            ChannelStatus::Moves
        };
        let picture = options.keep_pictures.then(|| pushed.picture());
        rows.push(ChannelSweep { channel: channel.name(), value, status, base, brows, overlay, picture });
    }
    Ok(Some(SweepReport { rows, rest: rest_picture }))
}
