//! A generation-checked slot arena: the bookkeeping behind the mock's handles.
//!
//! A released slot is reused, but its generation is bumped first, so a stale handle misses instead of silently
//! reaching the new occupant. That is the behaviour the API promises (`UnknownHandle`), so the mock must have it.

/// Slots addressed by `(index, generation)`.
#[derive(Debug)]
pub(crate) struct Arena<T> {
    slots: Vec<Slot<T>>,
    /// Indices of empty slots; the most recently freed is reused first, which keeps allocation order deterministic.
    free: Vec<u32>,
}

#[derive(Debug)]
struct Slot<T> {
    generation: u32,
    value: Option<T>,
}

impl<T> Default for Arena<T> {
    fn default() -> Self {
        Self { slots: Vec::new(), free: Vec::new() }
    }
}

impl<T> Arena<T> {
    /// Stores `value` and returns its `(index, generation)`. Generations start at 1, so `(0, 0)` is never live.
    pub(crate) fn insert(&mut self, value: T) -> (u32, u32) {
        if let Some(index) = self.free.pop() {
            let slot = &mut self.slots[index as usize];
            slot.value = Some(value);
            return (index, slot.generation);
        }
        // More than u32::MAX live resources would exhaust memory long before this wraps.
        #[allow(clippy::cast_possible_truncation)]
        let index = self.slots.len() as u32;
        self.slots.push(Slot { generation: 1, value: Some(value) });
        (index, 1)
    }

    pub(crate) fn get(&self, index: u32, generation: u32) -> Option<&T> {
        let slot = self.slots.get(index as usize)?;
        if slot.generation == generation { slot.value.as_ref() } else { None }
    }

    pub(crate) fn get_mut(&mut self, index: u32, generation: u32) -> Option<&mut T> {
        let slot = self.slots.get_mut(index as usize)?;
        if slot.generation == generation { slot.value.as_mut() } else { None }
    }

    /// Empties the slot (if the handle is live) and invalidates every handle that pointed at it.
    pub(crate) fn remove(&mut self, index: u32, generation: u32) -> Option<T> {
        let slot = self.slots.get_mut(index as usize)?;
        if slot.generation != generation {
            return None;
        }
        let value = slot.value.take()?;
        slot.generation = slot.generation.wrapping_add(1).max(1);
        self.free.push(index);
        Some(value)
    }

    /// Live values in slot order.
    pub(crate) fn iter(&self) -> impl Iterator<Item = &T> {
        self.slots.iter().filter_map(|s| s.value.as_ref())
    }

    pub(crate) fn len(&self) -> usize {
        self.slots.len() - self.free.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_reused_slot_rejects_the_old_handle() {
        let mut arena = Arena::default();
        let (index, generation) = arena.insert("first");
        assert_eq!(arena.get(index, generation), Some(&"first"));
        assert_eq!(arena.remove(index, generation), Some("first"));
        assert_eq!(arena.get(index, generation), None, "released");
        assert_eq!(arena.remove(index, generation), None, "a double release finds nothing");

        let (reused, newer) = arena.insert("second");
        assert_eq!(reused, index, "the slot is reused");
        assert_ne!(newer, generation, "under a new generation");
        assert_eq!(arena.get(index, generation), None, "the stale handle misses");
        assert_eq!(arena.get(reused, newer), Some(&"second"));
    }

    #[test]
    fn len_and_iter_count_live_values_only() {
        let mut arena = Arena::default();
        let ids: Vec<_> = (0..4).map(|v| arena.insert(v)).collect();
        arena.remove(ids[1].0, ids[1].1);
        assert_eq!(arena.len(), 3);
        assert_eq!(arena.iter().copied().collect::<Vec<_>>(), vec![0, 2, 3]);
        assert_eq!(arena.get(99, 1), None, "an index that never existed");
        assert_eq!(arena.get(0, 0), None, "generation 0 is never live");
    }
}
