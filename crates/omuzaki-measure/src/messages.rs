//! The sentences a measure refuses with (design 7.3), and the three refusals about the image itself. Each is a localised
//! message with a message id and typed arguments (ADR 0023), so a person reads it in their language and an agent
//! branches on its id. They are declared here, before the measures that raise them, so that every later task uses
//! these ids and none invents its own.

use std::sync::LazyLock;

use omuzaki_l10n_api::{Catalog, Locale, Resource, Text};

/// This crate's English file, rendered on its own. The app's catalog learns the file only when the crate calls
/// `register_locales!`, which needs every shipped language's file; until then the refusals are written in English from
/// here, so an error never reads as a last-resort `⟦id⟧`.
pub(crate) fn english(text: &Text) -> String {
    static ENGLISH: LazyLock<Catalog> = LazyLock::new(|| {
        let resource = Resource {
            package: env!("CARGO_PKG_NAME"),
            locale: "en",
            source: include_str!("../locales/en/omuzaki-measure.ftl"),
        };
        Catalog::from_sources(Locale::EN, &[], &[resource])
    });
    ENGLISH.render(text)
}

omuzaki_l10n_api::messages! {
    /// The eyes are closer together than the measure can read. Shown when a refusal is about the image's size.
    pub too_small = "measure-too-small" (distance: Num, minimum: Int)
        => "The eyes are { $distance } px apart; a measure needs at least { $minimum } px. Give a larger image.";

    /// A landmark given by the caller is not inside the image, so there is nothing under it to measure.
    pub landmark_outside = "measure-landmark-outside" (landmark: Id)
        => "The { $landmark } lies outside the image.";

    /// The mouth is above the eye line: the eyes were given the wrong way round, or the face is upside down.
    pub eyes_swapped = "measure-eyes-swapped" ()
        => "The mouth is above the eye line: the eyes may be swapped, or the face is upside down.";

    /// The line art is too faint to tell the face from the background, so the outline cannot be found.
    pub low_contrast = "measure-low-contrast" (contrast: Num, minimum: Int)
        => "The line art is too faint to bound the face: its contrast is { $contrast }, and at least { $minimum } is needed.";

    /// The skin fill escaped through a gap in the line art, so the face's outline would be the wrong shape.
    pub leak = "measure-leak" (sector: Id)
        => "The fill escaped through a gap in the line art near { $sector }. Render larger, or close the gap.";

    /// No outline sample reaches the chin, so the face has no bottom to measure to.
    pub no_chin = "measure-no-chin" ()
        => "No outline sample reaches the chin: the chin is hidden or outside the image.";

    /// Too much of the outline is missing to compare two faces fairly; the sectors name where.
    pub coverage = "measure-coverage" (sectors: Id)
        => "The outline is missing in { $sectors }: exclude less, or use a clearer view.";

    /// The likeness target was measured by another version of the measures, so its numbers cannot be compared.
    pub stale_target = "measure-stale-target" (target: Id)
        => "The likeness target { $target } was measured by another version of the measures: import its reference image again.";

    /// The character has no likeness target by that name.
    pub target_not_found = "measure-target-not-found" (target: Id)
        => "The character has no likeness target named { $target }. Set it first.";

    /// The image is larger than a measure reads, refused from its header before any pixels are allocated.
    pub image_too_large = "measure-image-too-large" (width: Int, height: Int, side_limit: Int, mebibyte_limit: Int)
        => "The image is { $width } × { $height } px. A measure reads at most { $side_limit } px a side and { $mebibyte_limit } MiB decoded.";

    /// The bytes are not a PNG or JPEG the decoder can read, or are another format. The reason is shown as written.
    pub image_unreadable = "measure-image-unreadable" (reason: Verbatim)
        => "The image could not be read: { $reason }.";

    /// The image is not sRGB, so its numbers would be wrong: a measure reads only sRGB, and refuses the rest rather than
    /// guess at them. The argument names what was found, such as a gamma or an embedded profile.
    pub image_colour_unsupported = "measure-image-colour-unsupported" (found: Verbatim)
        => "The image is not sRGB ({ $found }). Convert it to sRGB and try again.";
}
