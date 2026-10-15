//! Facts: what parts publish to each other, as an open map of typed values (ADR 0054 §3).
//!
//! A [`Fact`] is a typed value a part publishes for later parts, and the builder, to read. A new capability is a new
//! fact in a new file (plus [`register_fact!`]), not a new field on [`Publishes`](crate::Publishes) and
//! [`Published`](crate::Published) that every part, test and mock has to follow. Most facts merge by one of four rules
//! ([`Last`], [`Agree`], [`Append`], [`Keyed`]) and are one line with [`fact!`].
//!
//! Facts are build-time only: they live in the [`Assembler`](crate::Assembler)'s view while a character builds and are
//! never touched on the frame path.

use std::any::{Any, TypeId};
use std::collections::BTreeMap;
use std::marker::PhantomData;
use std::sync::{LazyLock, Mutex, PoisonError};

use crate::ids::PartId;

/// A typed value a part publishes for later parts, and the builder, to read.
///
/// `Value` and `Merged` must be `Clone` to be published ([`Publishes::put`](crate::Publishes::put) asks for it, not
/// this trait): the builder forks a part's output to cache it and clones the merged view it hands to parts built in
/// parallel. Share anything big behind an `Arc`.
pub trait Fact: 'static {
    /// Stable, unique name: errors, describe output and the link gate use it.
    const NAME: &'static str;
    /// What one part publishes.
    type Value: Send + Sync + 'static;
    /// What readers see after every earlier part's value is merged (Default = nothing published yet).
    type Merged: Default + Send + Sync + 'static;
    /// Merges one part's value in part order; an `Err(message)` becomes
    /// [`BuildError::Part`](crate::BuildError::Part) for that part.
    ///
    /// # Errors
    /// The message of a value that cannot be merged (two bind poses, two statures).
    fn merge(merged: &mut Self::Merged, part: &PartId, value: Self::Value) -> Result<(), String>;
}

/// A merge rule that facts share. Implemented by [`Last`], [`Agree`], [`Append`] and [`Keyed`]; [`fact!`] builds a
/// [`Fact`] from one.
pub trait Rule {
    /// What one part publishes.
    type Value: Send + Sync + 'static;
    /// What readers see.
    type Merged: Default + Send + Sync + 'static;
    /// Merges one value, see [`Fact::merge`].
    ///
    /// # Errors
    /// The message of a value the rule refuses.
    fn merge(merged: &mut Self::Merged, part: &PartId, value: Self::Value) -> Result<(), String>;
}

/// The last part that publishes a value wins; readers see `Option<V>`.
pub struct Last<V>(PhantomData<fn() -> V>);

impl<V: Send + Sync + 'static> Rule for Last<V> {
    type Value = V;
    type Merged = Option<V>;

    fn merge(merged: &mut Option<V>, _part: &PartId, value: V) -> Result<(), String> {
        *merged = Some(value);
        Ok(())
    }
}

/// Every part that publishes a value must publish an equal one (a different second value is an error); readers see
/// `Option<V>`.
pub struct Agree<V>(PhantomData<fn() -> V>);

impl<V: PartialEq + Send + Sync + 'static> Rule for Agree<V> {
    type Value = V;
    type Merged = Option<V>;

    fn merge(merged: &mut Option<V>, _part: &PartId, value: V) -> Result<(), String> {
        if merged.as_ref().is_some_and(|first| *first != value) {
            return Err("it publishes a different value than an earlier part: a character has one".to_owned());
        }
        *merged = Some(value);
        Ok(())
    }
}

/// Values are appended in part order; readers see `Vec<V>`.
pub struct Append<V>(PhantomData<fn() -> V>);

impl<V: Send + Sync + 'static> Rule for Append<V> {
    type Value = V;
    type Merged = Vec<V>;

    fn merge(merged: &mut Vec<V>, _part: &PartId, value: V) -> Result<(), String> {
        merged.push(value);
        Ok(())
    }
}

/// Values are keyed by the publishing part and a key of its own; publishing a key again replaces it. A part publishes
/// `(key, value)`; readers see `BTreeMap<(PartId, K), V>`.
pub struct Keyed<K, V>(PhantomData<fn() -> (K, V)>);

impl<K: Ord + Send + Sync + 'static, V: Send + Sync + 'static> Rule for Keyed<K, V> {
    type Value = (K, V);
    type Merged = BTreeMap<(PartId, K), V>;

    fn merge(merged: &mut Self::Merged, part: &PartId, (key, value): (K, V)) -> Result<(), String> {
        merged.insert((part.clone(), key), value);
        Ok(())
    }
}

/// Defines a fact that merges by a [`Rule`]:
///
/// ```ignore
/// omuzaki_char_api::fact! {
///     /// The hat a character wears.
///     pub HatFact = "hat", Last<Hat>;
/// }
/// omuzaki_char_api::register_fact!(HatFact);
/// ```
#[macro_export]
macro_rules! fact {
    ($(#[$meta:meta])* $vis:vis $name:ident = $label:literal, $rule:ty $(;)?) => {
        $(#[$meta])*
        $vis struct $name;

        impl $crate::Fact for $name {
            const NAME: &'static str = $label;
            type Value = <$rule as $crate::Rule>::Value;
            type Merged = <$rule as $crate::Rule>::Merged;

            fn merge(
                merged: &mut Self::Merged,
                part: &$crate::PartId,
                value: Self::Value,
            ) -> ::core::result::Result<(), ::std::string::String> {
                <$rule as $crate::Rule>::merge(merged, part, value)
            }
        }
    };
}

/// Records a fact so the link gate and the describe ops can list it. Put it next to the fact.
///
/// ```ignore
/// omuzaki_char_api::register_fact!(HatFact);
/// ```
#[macro_export]
macro_rules! register_fact {
    ($ty:ty) => {
        $crate::__record_registration!("fact", $ty);
    };
}

// ------------------------------------------------------------------------------------------------ storage

/// A published value with its type erased. Clone and thread-safe, so a [`Publishes`](crate::Publishes) forks.
pub(crate) trait Erased: Any + Send + Sync {
    fn clone_box(&self) -> Box<dyn Erased>;
    fn as_any(&self) -> &dyn Any;
    fn as_any_mut(&mut self) -> &mut dyn Any;
    fn into_any(self: Box<Self>) -> Box<dyn Any>;
}

impl<T: Clone + Send + Sync + 'static> Erased for T {
    fn clone_box(&self) -> Box<dyn Erased> {
        Box::new(self.clone())
    }
    fn as_any(&self) -> &dyn Any {
        self
    }
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }
    fn into_any(self: Box<Self>) -> Box<dyn Any> {
        self
    }
}

/// One erased value: a published one, or a fact's merged state.
pub(crate) struct Cell(Box<dyn Erased>);

impl Cell {
    pub(crate) fn new<T: Clone + Send + Sync + 'static>(value: T) -> Self {
        Self(Box::new(value))
    }

    pub(crate) fn get<T: 'static>(&self) -> Option<&T> {
        self.0.as_any().downcast_ref()
    }

    pub(crate) fn get_mut<T: 'static>(&mut self) -> Option<&mut T> {
        self.0.as_any_mut().downcast_mut()
    }

    fn into_inner<T: 'static>(self) -> Option<T> {
        self.0.into_any().downcast().ok().map(|b| *b)
    }
}

impl Clone for Cell {
    fn clone(&self) -> Self {
        Self(self.0.clone_box())
    }
}

/// Merges the values of one fact into `merged`, creating it as the `Default` on first use.
type MergeFn = fn(&mut BTreeMap<&'static str, Cell>, &PartId, Vec<Cell>) -> Result<(), String>;

/// Everything one part put for one fact, with the function that merges it.
#[derive(Clone)]
pub(crate) struct Slot {
    merge: MergeFn,
    pub(crate) values: Vec<Cell>,
}

impl Slot {
    pub(crate) fn new<F: Fact>() -> Self
    where
        F::Value: Clone,
        F::Merged: Clone,
    {
        Self { merge: merge_all::<F>, values: Vec::new() }
    }

    /// Merges this slot's values, in the order they were put.
    pub(crate) fn merge_into(self, merged: &mut BTreeMap<&'static str, Cell>, part: &PartId) -> Result<(), String> {
        (self.merge)(merged, part, self.values)
    }
}

fn merge_all<F: Fact>(merged: &mut BTreeMap<&'static str, Cell>, part: &PartId, values: Vec<Cell>) -> Result<(), String>
where
    F::Value: Clone,
    F::Merged: Clone,
{
    let clash = || format!("two facts are called '{}'", F::NAME);
    let cell = merged.entry(F::NAME).or_insert_with(|| Cell::new(F::Merged::default()));
    let cell = cell.get_mut::<F::Merged>().ok_or_else(clash)?;
    for value in values {
        F::merge(cell, part, value.into_inner::<F::Value>().ok_or_else(clash)?)?;
    }
    Ok(())
}

/// The `Default` of a merged type, made once and kept for the life of the process: what a reader sees when nothing was
/// published. One small allocation per fact type, at build time.
pub(crate) fn default_of<M: Default + Send + Sync + 'static>() -> &'static M {
    static DEFAULTS: LazyLock<Mutex<BTreeMap<TypeId, &'static (dyn Any + Send + Sync)>>> =
        LazyLock::new(|| Mutex::new(BTreeMap::new()));
    let mut defaults = DEFAULTS.lock().unwrap_or_else(PoisonError::into_inner);
    let any = *defaults.entry(TypeId::of::<M>()).or_insert_with(|| Box::leak(Box::new(M::default())));
    // The entry for `TypeId::of::<M>()` was made from an `M` above.
    match any.downcast_ref::<M>() {
        Some(m) => m,
        None => Box::leak(Box::new(M::default())),
    }
}
