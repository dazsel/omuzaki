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
use std::sync::OnceLock;

use crate::ids::PartId;

/// A typed value a part publishes for later parts, and the builder, to read.
///
/// A fact's values are cloned when a part output is forked and for builder views; facts are build-time only.
/// `Value` and `Merged` must therefore be `Clone` to be published ([`Publishes::put`](crate::Publishes::put) asks for
/// it, not this trait).
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

/// Records a fact, with its name and its type, so the link gate and the describe ops can list it ([`facts`]). Put it
/// next to a fact that a crate other than this one defines; the facts of this crate are not registered yet.
///
/// ```ignore
/// omuzaki_char_api::register_fact!(HatFact);
/// ```
#[macro_export]
macro_rules! register_fact {
    ($ty:ty) => {
        $crate::__inventory::submit! { $crate::fact::FactRegistration::of::<$ty>() }
        $crate::__record_registration!("fact", $ty);
    };
}

/// A fact recorded with [`register_fact!`]: its name and its type.
pub struct FactRegistration {
    /// [`Fact::NAME`].
    pub name: &'static str,
    /// The fact's type, so two facts that share a name are told apart.
    pub type_id: fn() -> TypeId,
    /// The fact's type name, for errors and listings.
    pub type_name: fn() -> &'static str,
}

impl FactRegistration {
    /// The registration of fact `F`.
    #[must_use]
    pub const fn of<F: Fact>() -> Self {
        Self { name: F::NAME, type_id: TypeId::of::<F>, type_name: std::any::type_name::<F> }
    }
}

inventory::collect!(FactRegistration);

/// Every registered fact, by name and then type name.
///
/// # Errors
/// Two registered facts that share a name but are different types, naming both. (A fact registered twice is one fact.)
pub fn facts() -> Result<Vec<&'static FactRegistration>, String> {
    let mut all: Vec<_> = inventory::iter::<FactRegistration>.into_iter().collect();
    all.sort_by_key(|r| (r.name, (r.type_name)()));
    all.dedup_by_key(|r| (r.name, (r.type_id)()));
    for pair in all.windows(2) {
        if let [a, b] = pair
            && a.name == b.name
        {
            return Err(clash_message(a.name, (a.type_name)(), (b.type_name)()));
        }
    }
    Ok(all)
}

/// The error for two different facts called `name`.
fn clash_message(name: &str, first: &str, second: &str) -> String {
    format!("two different facts are called '{name}': `{first}` and `{second}`; a fact's name must be unique")
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

/// Who a fact is: its name and its type. Two different facts that share a name never share a slot: the build refuses
/// the second one instead of merging it by the first one's rule.
#[derive(Clone, Copy)]
pub(crate) struct FactId {
    type_id: TypeId,
    type_name: &'static str,
}

impl FactId {
    pub(crate) fn of<F: Fact>() -> Self {
        Self { type_id: TypeId::of::<F>(), type_name: std::any::type_name::<F>() }
    }

    /// Whether this is fact `F`.
    pub(crate) fn is<F: Fact>(self) -> bool {
        self.type_id == TypeId::of::<F>()
    }

    /// The error for `other` claiming `name` when `self` already has it, or `None` when they are one fact.
    pub(crate) fn clash(self, other: Self, name: &str) -> Option<String> {
        (self.type_id != other.type_id).then(|| clash_message(name, self.type_name, other.type_name))
    }
}

/// A fact's merged state, with the fact it belongs to.
#[derive(Clone)]
pub(crate) struct Merged {
    pub(crate) id: FactId,
    pub(crate) cell: Cell,
}

/// Merges the values of one fact into `merged`, creating it as the `Default` on first use.
type MergeFn = fn(&mut BTreeMap<&'static str, Merged>, &PartId, Vec<Cell>) -> Result<(), String>;

/// Everything one part put for one fact, with the function that merges it.
#[derive(Clone)]
pub(crate) struct Slot {
    pub(crate) id: FactId,
    merge: MergeFn,
    pub(crate) values: Vec<Cell>,
}

impl Slot {
    pub(crate) fn new<F: Fact>() -> Self
    where
        F::Value: Clone,
        F::Merged: Clone,
    {
        Self { id: FactId::of::<F>(), merge: merge_all::<F>, values: Vec::new() }
    }

    /// Merges this slot's values, in the order they were put.
    pub(crate) fn merge_into(self, merged: &mut BTreeMap<&'static str, Merged>, part: &PartId) -> Result<(), String> {
        (self.merge)(merged, part, self.values)
    }
}

fn merge_all<F: Fact>(
    merged: &mut BTreeMap<&'static str, Merged>,
    part: &PartId,
    values: Vec<Cell>,
) -> Result<(), String>
where
    F::Value: Clone,
    F::Merged: Clone,
{
    let id = FactId::of::<F>();
    let entry = merged.entry(F::NAME).or_insert_with(|| Merged { id, cell: Cell::new(F::Merged::default()) });
    if let Some(message) = entry.id.clash(id, F::NAME) {
        return Err(message);
    }
    // The identity matched, so the types do; a failed downcast would be a bug in this module.
    let lost = || format!("the merged value of fact '{}' has the wrong type", F::NAME);
    let cell = entry.cell.get_mut::<F::Merged>().ok_or_else(lost)?;
    for value in values {
        F::merge(cell, part, value.into_inner::<F::Value>().ok_or_else(lost)?)?;
    }
    Ok(())
}

/// The empty values of the facts nothing published, for one [`Published`](crate::Published): what a reader sees when a
/// fact is missing. Each view makes its own, so a `Merged` with interior mutability can't carry a change from one build
/// into another, or from one fact into another with the same merged type.
///
/// An append-only list of cells, one per missing fact, keyed by the fact. A read walks the list and allocates only the
/// first time a fact is missing here, never again. Published facts never reach it. Cloning gives the clone an empty
/// list, so no default is ever shared between two views.
///
/// A `Default` may read the view it is made for (through a `static`), so it is made outside any cell, never inside a
/// one-time initialiser that it could re-enter. Two threads that both miss a fact may each make one; the first one
/// stored wins and the other is dropped.
#[derive(Default)]
pub(crate) struct Defaults {
    head: OnceLock<Box<Node>>,
}

struct Node {
    fact: TypeId,
    value: Box<dyn Any + Send + Sync>,
    next: OnceLock<Box<Node>>,
}

impl Defaults {
    /// The empty value of fact `F` in this view, made on the first read.
    pub(crate) fn of<F: Fact>(&self) -> &F::Merged {
        let key = TypeId::of::<F>();
        // A default made for an empty cell that another reader filled first: kept for the next empty cell.
        let mut spare: Option<Box<Node>> = None;
        let mut link = &self.head;
        loop {
            let Some(node) = link.get() else {
                let fresh = spare.take().unwrap_or_else(|| {
                    Box::new(Node { fact: key, value: Box::new(F::Merged::default()), next: OnceLock::new() })
                });
                if let Err(lost) = link.set(fresh) {
                    spare = Some(lost);
                }
                continue;
            };
            // The key is the fact's own type, so a match always has the fact's merged type; a miss moves on.
            if node.fact == key
                && let Some(m) = node.value.downcast_ref::<F::Merged>()
            {
                return m;
            }
            link = &node.next;
        }
    }
}

impl Clone for Defaults {
    /// A clone starts empty: it makes its own defaults, sharing none with the original.
    fn clone(&self) -> Self {
        Self::default()
    }
}
