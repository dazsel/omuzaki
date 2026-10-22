//! The face's expression baker, found through a registry (spec v3 §19 FE-09, decision G13).
//!
//! `io.export` (in `omuzaki-io-ops`) bakes the character's face into a scene, but the baker is implemented by a face impl
//! crate, and impl crates cannot call each other. So the baker is found the way `omuzaki-io-api` finds importers and
//! exporters: a factory self-registers with [`register_face_baker!`] next to its type, and [`face_baker`] returns the
//! one with the highest [`FaceBakerFactory::priority`]. Nothing edits a central list.

use std::sync::OnceLock;

use omuzaki_core::glam::Vec3;
use omuzaki_io_api::{ExpressionBaker, IoError};

use crate::{FaceProfile, FaceProjection};

/// What a factory needs to bake one character's face.
#[derive(Clone, Copy, Debug)]
pub struct BakerInput<'a> {
    /// The face design to bake.
    pub profile: &'a FaceProfile,
    /// Where the painted layers land on the head.
    pub projection: FaceProjection,
    /// The head centre in bind space: the head-local origin that [`FaceProjection::planar`] is measured from.
    pub head_centre: Vec3,
    /// Bind-space metres per head-local unit: the character's stature against its template's (and any export scale),
    /// 1 at the template's size. The projection stays in head-local units, so the painted features keep their pixels.
    pub scale: f32,
}

/// Builds the face's [`ExpressionBaker`] for one character. Implement it on a stateless type and register that type
/// with [`register_face_baker!`].
pub trait FaceBakerFactory: Send + Sync {
    /// Which factory wins when several are registered: the highest value. Two factories with the same priority are an
    /// error (see [`face_baker`]), so a replacement takes a higher one.
    fn priority(&self) -> i32 {
        0
    }

    /// A baker for `input`.
    ///
    /// # Errors
    ///
    /// An [`IoError`] when this factory can't bake `input`. It is a value, never a panic.
    fn create(&self, input: BakerInput<'_>) -> Result<Box<dyn ExpressionBaker>, IoError>;
}

/// One factory's registration, collected by `inventory` at link time. [`register_face_baker!`] builds it.
pub struct FaceBakerRegistration {
    /// The factory's name as written in the macro, for the conflict message.
    pub name: &'static str,
    /// The factory itself. Factories are stateless, so a `'static` value is enough.
    pub factory: &'static dyn FaceBakerFactory,
}

impl FaceBakerRegistration {
    /// The registration for `factory`. A `const fn` so [`register_face_baker!`] works at link time.
    pub const fn of<T: FaceBakerFactory>(name: &'static str, factory: &'static T) -> Self {
        Self { name, factory }
    }
}

inventory::collect!(FaceBakerRegistration);

/// Two registered factories share a priority, so neither can be chosen. Names both, so the fix is obvious.
#[derive(Clone, Copy, Debug, PartialEq, Eq, thiserror::Error)]
#[error("face bakers {first} and {second} both have priority {priority}; give one a different priority()")]
pub struct FaceBakerConflict {
    /// The priority both factories claim.
    pub priority: i32,
    /// The name of the first factory, in name order.
    pub first: &'static str,
    /// The name of the second factory, in name order.
    pub second: &'static str,
}

/// Picks the highest-priority registration among `candidates`, or `None` when there are none.
///
/// Priorities must be unique: any two that match are a [`FaceBakerConflict`], wherever they sit in the order, so the
/// answer never depends on link order. Kept free of the global registry so the rule is testable.
fn select<'a>(
    candidates: impl IntoIterator<Item = &'a FaceBakerRegistration>,
) -> Result<Option<&'a FaceBakerRegistration>, FaceBakerConflict> {
    let mut ranked: Vec<(i32, &FaceBakerRegistration)> =
        candidates.into_iter().map(|r| (r.factory.priority(), r)).collect();
    // Highest priority first. The name order only decides which clashing pair an error names; it is deterministic.
    ranked.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.name.cmp(b.1.name)));
    if let Some(pair) = ranked.windows(2).find(|w| w[0].0 == w[1].0) {
        return Err(FaceBakerConflict { priority: pair[0].0, first: pair[0].1.name, second: pair[1].1.name });
    }
    Ok(ranked.first().map(|&(_, registration)| registration))
}

/// The face's baker factory: the highest-priority registered one, or `None` when nothing is registered.
///
/// The registry is read once, on first use; the answer, or the conflict, is kept for every later call.
///
/// # Errors
///
/// [`FaceBakerConflict`] when two registered factories share a priority. Neither can be chosen, so the caller gets
/// both names and decides, rather than receiving a silent pick.
pub fn face_baker() -> Result<Option<&'static dyn FaceBakerFactory>, FaceBakerConflict> {
    static CHOSEN: OnceLock<Result<Option<&'static dyn FaceBakerFactory>, FaceBakerConflict>> = OnceLock::new();
    *CHOSEN.get_or_init(|| select(inventory::iter::<FaceBakerRegistration>).map(|found| found.map(|r| r.factory)))
}

/// Registers a face baker factory: `register_face_baker!(MockFaceBakerFactory);`. Put it next to the type. The value
/// must be a constant `'static` expression, normally a unit struct, because it is stored in a link-time registry.
#[macro_export]
macro_rules! register_face_baker {
    ($factory:expr) => {
        $crate::__inventory::submit! { $crate::FaceBakerRegistration::of(stringify!($factory), &$factory) }
        $crate::__record_registration!("face_baker", $factory);
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A factory with a fixed priority; it never bakes.
    struct Fake(i32);

    impl FaceBakerFactory for Fake {
        fn priority(&self) -> i32 {
            self.0
        }

        fn create(&self, _input: BakerInput<'_>) -> Result<Box<dyn ExpressionBaker>, IoError> {
            Err(IoError::Internal("test factory".into()))
        }
    }

    #[test]
    fn nothing_registered_selects_nothing() {
        assert!(select(std::iter::empty::<&FaceBakerRegistration>()).is_ok_and(|found| found.is_none()));
    }

    #[test]
    fn the_highest_priority_wins_whatever_the_order() {
        let forward = [FaceBakerRegistration::of("low", &Fake(1)), FaceBakerRegistration::of("high", &Fake(5))];
        let backward = [FaceBakerRegistration::of("high", &Fake(5)), FaceBakerRegistration::of("low", &Fake(1))];
        for regs in [&forward[..], &backward[..]] {
            let chosen = select(regs).expect("no tie").expect("one wins");
            assert_eq!(chosen.name, "high");
        }
    }

    #[test]
    fn equal_priorities_are_a_conflict_in_either_order() {
        let forward = [FaceBakerRegistration::of("a", &Fake(3)), FaceBakerRegistration::of("b", &Fake(3))];
        let backward = [FaceBakerRegistration::of("b", &Fake(3)), FaceBakerRegistration::of("a", &Fake(3))];
        let expected = FaceBakerConflict { priority: 3, first: "a", second: "b" };
        assert_eq!(select(&forward).err(), Some(expected));
        assert_eq!(select(&backward).err(), Some(expected));
        let message = select(&forward).err().map(|c| c.to_string());
        assert_eq!(
            message.as_deref(),
            Some("face bakers a and b both have priority 3; give one a different priority()")
        );
    }

    #[test]
    fn a_tie_below_the_winner_is_still_a_conflict() {
        let regs = [
            FaceBakerRegistration::of("c", &Fake(9)),
            FaceBakerRegistration::of("x", &Fake(1)),
            FaceBakerRegistration::of("y", &Fake(1)),
        ];
        assert_eq!(select(&regs).err(), Some(FaceBakerConflict { priority: 1, first: "x", second: "y" }));
    }
}
