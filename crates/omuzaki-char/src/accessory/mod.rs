//! Accessories (CB-43): what is worn on or fixed to the character, built on the attachment framework.
//!
//! | Module | Kinds |
//! |---|---|
//! | [`attach`] | where an accessory is fixed: a bone or an anchor, with an offset (and the `accessory.list_anchors` query) |
//! | [`rigid`] | `choker`, `bell`, `charm`, `hairpin` and `halo`: small rigid jewellery, the bell, charm and halo on a two-bone spring chain |
//! | [`horns`] | `horns`: swept back, ram, oni and antler, with an optional ember-crack glow |
//! | [`headphones`] | `headphones`: a band over the hair, cups at the ears, optional cat ears and mic |
//! | [`hat`] | `hat`: beret, cap, witch hat, beanie and crown |
//! | [`glasses`] | `glasses`: round, square, cat-eye and sunglasses |
//! | [`tail`] | `tail`: fox, cat, dragon and bunny on a spring-bone chain |
//! | [`wings`] | `wings`: feathered, bat, fairy and mechanical, on a frame that folds through a custom channel |
//!
//! The kinds after `rigid` share `shells` (private): material slots as data, shells placed by the attachment, per-vertex
//! skinning, spring chains and the recolour.

pub mod attach;
pub mod glasses;
pub mod hat;
pub mod headphones;
pub mod horns;
pub mod rigid;
mod shells;
pub mod tail;
pub mod wings;
