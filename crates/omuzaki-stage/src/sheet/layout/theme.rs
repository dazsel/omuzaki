//! The sheet's colours.

use omuzaki_core::colour::Colour;

/// The colours of a sheet. One accent, a warm neutral paper and an ink that clears 14:1 against it, so the sheet
/// prints and photographs well and the art is the only saturated thing on it.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct Theme {
    /// The paper. Swatch contrast ratios are measured against this colour.
    pub background: Colour,
    /// Titles and body text.
    pub ink: Colour,
    /// Secondary text: labels, units, captions.
    pub muted: Colour,
    /// Rules and outlines.
    pub hairline: Colour,
    /// The quiet fill behind a tile.
    pub panel: Colour,
    /// Measurement graphics: rulers and their readouts.
    pub accent: Colour,
}

impl Theme {
    /// Dark ink on warm white: the default for print and sharing.
    pub const fn light() -> Self {
        Self {
            background: Colour::hex(0x00fa_f9f6),
            ink: Colour::hex(0x001b_1d22),
            muted: Colour::hex(0x0062_6672),
            hairline: Colour::hex(0x00d8_d6cf),
            panel: Colour::hex(0x00f0_eee8),
            accent: Colour::hex(0x0020_78b4),
        }
    }

    /// Light ink on a deep blue-grey, for dark-mode boards.
    pub const fn dark() -> Self {
        Self {
            background: Colour::hex(0x0016_181d),
            ink: Colour::hex(0x00ec_edef),
            muted: Colour::hex(0x00a0_a4af),
            hairline: Colour::hex(0x002e_313a),
            panel: Colour::hex(0x001e_2129),
            accent: Colour::hex(0x0066_b4e8),
        }
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self::light()
    }
}
