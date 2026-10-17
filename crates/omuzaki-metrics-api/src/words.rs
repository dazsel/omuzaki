//! Packing a section into the `u64` words of a [`SeqCell`](crate::cell::SeqCell) and back.
//!
//! Floats travel as their bit patterns, so a value (NaN payloads included) comes back unchanged.

/// Appends fields to a fixed-size word array in order.
pub(crate) struct Put<const N: usize> {
    words: [u64; N],
    next: usize,
}

impl<const N: usize> Put<N> {
    pub(crate) fn new() -> Self {
        Self { words: [0; N], next: 0 }
    }

    pub(crate) fn u64(&mut self, v: u64) -> &mut Self {
        // A section that writes more fields than it declared is a bug the round-trip tests catch.
        debug_assert!(self.next < N, "more fields than the section's word count");
        if let Some(slot) = self.words.get_mut(self.next) {
            *slot = v;
        }
        self.next += 1;
        self
    }

    pub(crate) fn f32(&mut self, v: f32) -> &mut Self {
        self.u64(u64::from(v.to_bits()))
    }

    pub(crate) fn f64(&mut self, v: f64) -> &mut Self {
        self.u64(v.to_bits())
    }

    pub(crate) fn finish(&self) -> [u64; N] {
        debug_assert_eq!(self.next, N, "fewer fields than the section's word count");
        self.words
    }
}

/// Reads fields back from a word slice in the order [`Put`] wrote them.
pub(crate) struct Get<'a> {
    words: &'a [u64],
    next: usize,
}

impl<'a> Get<'a> {
    pub(crate) fn new(words: &'a [u64]) -> Self {
        Self { words, next: 0 }
    }

    pub(crate) fn u64(&mut self) -> u64 {
        debug_assert!(self.next < self.words.len(), "more fields than the section's word count");
        let v = self.words.get(self.next).copied().unwrap_or(0);
        self.next += 1;
        v
    }

    /// A `u64` that was written from a `u32`; `None` if the high half is set (a corrupt word).
    pub(crate) fn u32(&mut self) -> Option<u32> {
        u32::try_from(self.u64()).ok()
    }

    /// An `f32` written by [`Put::f32`]; `None` if the high half is set (a corrupt word).
    pub(crate) fn f32(&mut self) -> Option<f32> {
        self.u32().map(f32::from_bits)
    }

    pub(crate) fn f64(&mut self) -> f64 {
        f64::from_bits(self.u64())
    }
}
