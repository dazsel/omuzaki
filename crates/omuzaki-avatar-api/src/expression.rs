//! Expressions and how channels drive them (design D3).
//!
//! An [`AvatarExpression`] is the runtime form of a VRM expression: morph, colour and UV binds, whether it is binary,
//! and how it overrides the procedural expressions. Binds name what they move in the avatar's own terms (a mesh index,
//! a material name), and colour and UV binds carry the value they start from, so the evaluator needs nothing but the
//! avatar to turn a weight into deltas.
//!
//! The channel table (`omuzaki-anim-api::channel`) is the only thing producers write. [`ChannelDrivers`] says which
//! expression each channel drives: one channel drives at most one expression, and several channels may drive one
//! expression (the largest weight wins).

use omuzaki_anim_api::{CHANNEL_CAPACITY, Channel, Region};
use omuzaki_char_api::MaterialName;
use omuzaki_core::glam::{Vec2, Vec4};
use omuzaki_io_api::{ColorBindKind, ExpressionName, Override, PresetExpression};

/// An index into [`ImportedAvatar::expressions`](crate::avatar::ImportedAvatar::expressions).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ExprIx(pub u16);

impl ExprIx {
    /// The index as a `usize`, for slicing.
    pub const fn index(self) -> usize {
        self.0 as usize
    }
}

/// Moves one morph target of one mesh as the expression rises.
#[derive(Copy, Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct MorphBind {
    /// The mesh, an index into [`ImportedAvatar::meshes`](crate::avatar::ImportedAvatar::meshes).
    pub mesh: u16,
    /// The target, an index into that mesh's morph set.
    pub target: u16,
    /// The target's weight at expression weight 1 (VRM `weight`, `0..=1`).
    pub weight: f32,
}

impl MorphBind {
    /// A bind of `target` on `mesh` reaching `weight`.
    pub const fn new(mesh: u16, target: u16, weight: f32) -> Self {
        Self { mesh, target, weight }
    }
}

/// Blends one material colour from `base` toward `target`: at weight `w` the colour moves by `(target − base) · w`
/// (VRM 1.0 `expressions.md`, "Expression update algorithm").
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct ColourBind {
    /// The material.
    pub material: MaterialName,
    /// Which colour of it.
    pub kind: ColorBindKind,
    /// The colour the material has with no expression active (linear RGBA, as the file stores it).
    pub base: Vec4,
    /// The colour at weight 1 (linear RGBA; the alpha is ignored for colours that have none).
    pub target: Vec4,
}

impl ColourBind {
    /// A bind moving `kind` of `material` from `base` toward `target`.
    pub fn new(material: MaterialName, kind: ColorBindKind, base: Vec4, target: Vec4) -> Self {
        Self { material, kind, base, target }
    }

    /// What weight 1 adds to the colour: `target − base`.
    pub fn delta(&self) -> Vec4 {
        self.target - self.base
    }
}

/// Moves the UV scale and offset of a material's UV-addressed textures from its base transform toward a target, like
/// a colour bind (how painted eyes and mouths switch atlas cells).
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct UvBind {
    /// The material.
    pub material: MaterialName,
    /// The material's scale with no expression active.
    pub base_scale: Vec2,
    /// The material's offset with no expression active.
    pub base_offset: Vec2,
    /// The scale at weight 1.
    pub scale: Vec2,
    /// The offset at weight 1.
    pub offset: Vec2,
}

impl UvBind {
    /// A bind moving `material` from the identity transform to `scale` and `offset`.
    pub fn new(material: MaterialName, scale: Vec2, offset: Vec2) -> Self {
        Self { material, base_scale: Vec2::ONE, base_offset: Vec2::ZERO, scale, offset }
    }

    /// The same bind starting from the material's own transform instead of the identity (builder style).
    #[must_use]
    pub fn from_base(mut self, base_scale: Vec2, base_offset: Vec2) -> Self {
        self.base_scale = base_scale;
        self.base_offset = base_offset;
        self
    }
}

/// The three procedural classes an expression can override (VRM `overrideBlink`, `overrideLookAt`, `overrideMouth`).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Procedural {
    /// The blink presets, and the `eyeBlink*` channels.
    Blink,
    /// The look presets, and the `Gaze` region (`eyeLook*`).
    LookAt,
    /// The lip-sync presets `aa`…`oh`, and the `Mouth` region.
    Mouth,
}

impl Procedural {
    /// All classes.
    pub const ALL: [Self; 3] = [Self::Blink, Self::LookAt, Self::Mouth];

    /// The class of an expression called `name`, or `None` for an expression nothing overrides. The VRM classes are the
    /// presets; the spec leaves custom expressions to the application, and Omuzaki extends the classes to the expressions
    /// named like the `ARKit` channels, by region: `eyeBlink*` with blink, `Gaze` with look-at, `Mouth` with lip sync.
    pub fn of(name: &ExpressionName) -> Option<Self> {
        match name {
            ExpressionName::Preset(preset) => match preset {
                PresetExpression::Blink | PresetExpression::BlinkLeft | PresetExpression::BlinkRight => {
                    Some(Self::Blink)
                }
                PresetExpression::LookUp
                | PresetExpression::LookDown
                | PresetExpression::LookLeft
                | PresetExpression::LookRight => Some(Self::LookAt),
                PresetExpression::Aa
                | PresetExpression::Ih
                | PresetExpression::Ou
                | PresetExpression::Ee
                | PresetExpression::Oh => Some(Self::Mouth),
                PresetExpression::Happy
                | PresetExpression::Angry
                | PresetExpression::Sad
                | PresetExpression::Relaxed
                | PresetExpression::Surprised
                | PresetExpression::Neutral => None,
            },
            ExpressionName::Custom(custom) => {
                let channel = Channel::from_name(custom.as_str()).filter(|c| c.is_arkit())?;
                match channel.region() {
                    _ if channel == Channel::EYE_BLINK_LEFT || channel == Channel::EYE_BLINK_RIGHT => Some(Self::Blink),
                    Region::Gaze => Some(Self::LookAt),
                    Region::Mouth => Some(Self::Mouth),
                    _ => None,
                }
            }
        }
    }
}

/// How an expression affects each procedural class while it is active.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq)]
pub struct Overrides {
    /// Effect on the blink class.
    pub blink: Override,
    /// Effect on the look-at class.
    pub look_at: Override,
    /// Effect on the mouth class.
    pub mouth: Override,
}

impl Overrides {
    /// Overrides with the given effect on each class.
    pub const fn new(blink: Override, look_at: Override, mouth: Override) -> Self {
        Self { blink, look_at, mouth }
    }

    /// The effect on `class`.
    pub const fn get(&self, class: Procedural) -> Override {
        match class {
            Procedural::Blink => self.blink,
            Procedural::LookAt => self.look_at,
            Procedural::Mouth => self.mouth,
        }
    }
}

/// One expression of an imported avatar: its name as the file has it, its binds, and its override rules.
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub struct AvatarExpression {
    /// The name in the file (a preset, or a custom name such as a perfect-sync `jawOpen`), kept for reports, hotkeys
    /// and export.
    pub name: ExpressionName,
    /// Morph targets driven.
    pub morph: Vec<MorphBind>,
    /// Material colours driven.
    pub colour: Vec<ColourBind>,
    /// Texture transforms driven.
    pub uv: Vec<UvBind>,
    /// Values above 0.5 count as 1 and the rest as 0, before the expression overrides anything; as a target it shows
    /// only 0 or 1.
    pub binary: bool,
    /// How it overrides the procedural classes while active.
    pub overrides: Overrides,
}

impl AvatarExpression {
    /// An expression called `name` that drives nothing yet, is not binary and overrides nothing.
    pub fn new(name: ExpressionName) -> Self {
        Self {
            name,
            morph: Vec::new(),
            colour: Vec::new(),
            uv: Vec::new(),
            binary: false,
            overrides: Overrides::default(),
        }
    }

    /// Adds a morph bind (builder style).
    #[must_use]
    pub fn with_morph(mut self, bind: MorphBind) -> Self {
        self.morph.push(bind);
        self
    }

    /// Adds a colour bind (builder style).
    #[must_use]
    pub fn with_colour(mut self, bind: ColourBind) -> Self {
        self.colour.push(bind);
        self
    }

    /// Adds a UV bind (builder style).
    #[must_use]
    pub fn with_uv(mut self, bind: UvBind) -> Self {
        self.uv.push(bind);
        self
    }

    /// Makes it binary (builder style).
    #[must_use]
    pub fn binary(mut self) -> Self {
        self.binary = true;
        self
    }

    /// Sets its overrides (builder style).
    #[must_use]
    pub fn with_overrides(mut self, overrides: Overrides) -> Self {
        self.overrides = overrides;
        self
    }

    /// The procedural class it belongs to as a target of other expressions' overrides ([`Procedural::of`]).
    pub fn procedural(&self) -> Option<Procedural> {
        Procedural::of(&self.name)
    }
}

/// Which expression each channel drives: an [`ExprIx`] per channel slot, or none.
#[derive(Clone, PartialEq, Eq)]
pub struct ChannelDrivers([Option<ExprIx>; CHANNEL_CAPACITY]);

impl Default for ChannelDrivers {
    fn default() -> Self {
        Self([None; CHANNEL_CAPACITY])
    }
}

impl ChannelDrivers {
    /// No channel drives anything.
    pub fn new() -> Self {
        Self::default()
    }

    /// The expression `channel` drives, if any.
    pub fn get(&self, channel: Channel) -> Option<ExprIx> {
        self.0[channel.index()]
    }

    /// Makes `channel` drive `expression`, or nothing.
    pub fn set(&mut self, channel: Channel, expression: Option<ExprIx>) {
        self.0[channel.index()] = expression;
    }

    /// Sets one driver (builder style).
    #[must_use]
    pub fn with(mut self, channel: Channel, expression: ExprIx) -> Self {
        self.set(channel, Some(expression));
        self
    }

    /// Every channel that drives something, in channel order, with what it drives.
    pub fn iter(&self) -> impl Iterator<Item = (Channel, ExprIx)> + '_ {
        self.0.iter().enumerate().filter_map(|(i, e)| Some((Channel::from_index(i)?, (*e)?)))
    }
}

impl std::fmt::Debug for ChannelDrivers {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_map().entries(self.iter().map(|(c, e)| (c, e.0))).finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn name(n: &str) -> ExpressionName {
        ExpressionName::parse(n).unwrap()
    }

    #[test]
    fn presets_and_arkit_names_fall_into_the_vrm_classes() {
        for (n, class) in [
            ("blink", Some(Procedural::Blink)),
            ("blinkLeft", Some(Procedural::Blink)),
            ("eyeBlinkRight", Some(Procedural::Blink)),
            ("lookUp", Some(Procedural::LookAt)),
            ("eyeLookInLeft", Some(Procedural::LookAt)),
            ("aa", Some(Procedural::Mouth)),
            ("jawOpen", Some(Procedural::Mouth)),
            ("cheekPuff", Some(Procedural::Mouth)),
            ("happy", None),
            ("neutral", None),
            ("eyeSquintLeft", None),
            ("browInnerUp", None),
            ("Puff", None),
            ("eyesHeart", None),
        ] {
            assert_eq!(Procedural::of(&name(n)), class, "{n}");
        }
    }

    #[test]
    fn drivers_list_what_is_set_in_channel_order() {
        let mut d = ChannelDrivers::new().with(Channel::JAW_OPEN, ExprIx(2)).with(Channel::BROW_DOWN_LEFT, ExprIx(0));
        assert_eq!(d.get(Channel::JAW_OPEN), Some(ExprIx(2)));
        assert_eq!(
            d.iter().collect::<Vec<_>>(),
            [(Channel::BROW_DOWN_LEFT, ExprIx(0)), (Channel::JAW_OPEN, ExprIx(2))]
        );
        d.set(Channel::JAW_OPEN, None);
        assert_eq!(d.iter().count(), 1);
    }

    #[test]
    fn a_colour_bind_adds_the_difference_from_its_base() {
        let m = MaterialName::new("skin").unwrap();
        let b = ColourBind::new(m, ColorBindKind::Color, Vec4::splat(0.25), Vec4::ONE);
        assert_eq!(b.delta(), Vec4::splat(0.75));
    }
}
