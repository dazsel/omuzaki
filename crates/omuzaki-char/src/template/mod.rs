//! Body templates: a skeleton, proportions and section curves that every body part is lofted from.
//!
//! A [`Template`] turns the body part's numbers ([`Proportions`], [`BodyShape`], [`HandParams`]) into a [`BodyLayout`]:
//! where every body joint sits and how wide the body is along each region, as a [`BodyFit`](omuzaki_char_api::BodyFit)
//! that garments, the skeleton and the body mesh all read. Templates register themselves with
//! [`register_template!`](crate::register_template), the same way part kinds do, so adding one means adding one file.
//!
//! **One head frame.** Every template shares the head: the head part's metres (the default cranium is 0.38 m from chin
//! to crown). A template scales the *body* against that head, so a template is described in head units
//! ([`Template::heads_tall`]). **Real size.** Templates are authored against that one head, so their authored heights
//! differ from real people's; each declares the stature it stands for ([`Template::default_stature_m`]) and the body part
//! publishes [`Template::stature`], so the build scales the body and the head uniformly to real metres before anything
//! else is built (`omuzaki_char_api::stature`).
//!
//! | Module | Holds |
//! |---|---|
//! | [`layout`] | [`BodyLayout`], the data tables a template is written as, and the builder that applies the sliders |
//! | [`registry`] | self-registration and lookup by id |
//! | [`soft_sd`] | the default chibi template (about 2.5 heads tall) |
//! | [`mini`], [`standard`], [`tall`] | further templates |
//! | [`shape`] | the body-shape space (CB-04): the five [`BodyShape`] axes as deformations of any template, and the presets in `library/body-shapes/` |
//! | [`skeleton`], [`conformance`] | skeleton assembly and the shared rules |

use omuzaki_char_api::{BodyParams, BodyRegion, BodyShape, HandParams, HeadParams, Proportions, Stature};
use omuzaki_face_api::FaceProfile;

use crate::kit::KitError;
use layout::BodyLayout;

pub mod conformance;
pub mod layout;
pub mod mini;
pub mod registry;
pub mod shape;
pub mod skeleton;
pub mod soft_sd;
pub mod standard;
pub mod tall;

/// The head, body and face numbers a new character of a template starts from (what `character.create` writes).
///
/// A template whose defaults equal the part kinds' own (the soft-sd template does) returns [`Default::default`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TemplateDefaults {
    /// The head part's parameters.
    pub head: HeadParams,
    /// The body part's parameters: proportions, body shape, hands and skin.
    pub body: BodyParams,
    /// The face design.
    pub face: FaceProfile,
}

/// A body template (CB-01): fixes the skeleton's layout and the body's section curves at its default proportions.
///
/// Implementations are stateless (`&self` carries no data beyond the template's tables) and deterministic: the same
/// numbers always give the same layout, on every platform.
pub trait Template: Send + Sync + 'static {
    /// The template's id, a valid [`BodyTemplateId`](omuzaki_char_api::BodyTemplateId) slug (`soft-sd`).
    fn id(&self) -> &'static str;

    /// One line: what the template is, for pickers and docs.
    fn summary(&self) -> &'static str;

    /// Total height in head units (crown to sole over chin to crown), as the template declares it.
    fn heads_tall(&self) -> f64;

    /// The height in metres of the person this template stands for, with neutral proportions and body shape: the
    /// stature a new character of the template is built at (0.966 for the chibi, 1.714 for the tall adult; see
    /// `docs/research/2026-10-template-stature.md`).
    fn default_stature_m(&self) -> f64;

    /// The template's height crown to sole in its authored space, at neutral sliders and the default head. This is what
    /// [`default_stature_m`](Self::default_stature_m) is measured against; `0` if the template builds no layout.
    fn authored_height_m(&self) -> f64 {
        self.authored_height_of(&self.layout(&Proportions::default(), &BodyShape::default(), &HandParams::default()))
    }

    /// Crown to sole of a character built from `layout` and the template's default head: the head rides the neck, so the
    /// crown follows the head base when the sliders move it. `0` for an empty layout.
    fn authored_height_of(&self, layout: &BodyLayout) -> f64 {
        let Some(base) = layout.head_base() else { return 0.0 };
        let Some(foot) = layout.section64(BodyRegion::FootL, 0.5) else { return 0.0 };
        let cranium = self.defaults().head.cranium;
        // An unset centre rides the head base (`head::part::seated`); a written one stays where it is written.
        let centre = cranium.raised_centre(base.y - soft_sd::TABLE.joints.head_base.y);
        let crown = centre[1] + cranium.half_height_up;
        (crown - (foot.centre.y - foot.radii.y)).max(0.0)
    }

    /// The uniform factor that takes the template's authored space to real size: the default stature over the
    /// authored height. The same for every character of the template, so a body-shape `height` of +12 % makes the body
    /// 12 % taller, and the head, which keeps its size, makes the whole figure a little less.
    fn stature_scale(&self) -> f64 {
        let authored = self.authored_height_m();
        if authored > 0.0 { self.default_stature_m() / authored } else { 1.0 }
    }

    /// The stature of the character built from `layout`: the template's scale and the height that gives in metres
    /// (`None` when the layout is empty). The body part publishes this.
    fn stature(&self, layout: &BodyLayout) -> Option<Stature> {
        let scale = self.stature_scale();
        let height = self.authored_height_of(layout) * scale;
        #[allow(clippy::cast_possible_truncation)] // metres, well inside f32
        let stature = Stature { scale: scale as f32, height_m: height as f32 };
        stature.check().ok().map(|()| stature)
    }

    /// The layout for these numbers, or the reason it cannot be built.
    ///
    /// Out-of-range sliders are clamped to their valid range and NaN reads as the neutral value, so for the shipped
    /// tables this does not fail; a custom table with a mistake in it does, and says what.
    ///
    /// # Errors
    /// [`KitError`] if the template's tables do not describe a valid layout (a repeated curve knot, a region of zero
    /// length).
    fn try_layout(
        &self,
        proportions: &Proportions,
        shape: &BodyShape,
        hand: &HandParams,
    ) -> Result<BodyLayout, KitError>;

    /// The layout for these numbers. A template that cannot build one has *no* layout ([`BodyLayout::empty`]): every
    /// joint and section reads `None`, so a consumer reports a missing body instead of fitting to a broken one. Call
    /// [`Template::try_layout`] to learn why.
    fn layout(&self, proportions: &Proportions, shape: &BodyShape, hand: &HandParams) -> BodyLayout {
        self.try_layout(proportions, shape, hand).unwrap_or_else(|_| BodyLayout::empty())
    }

    /// The starting numbers of a new character on this template.
    fn defaults(&self) -> TemplateDefaults;
}
