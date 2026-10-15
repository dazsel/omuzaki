//! Slot ownership: a slot belongs to the worker until it posts it and to the host until the host has finished with it
//! and sends the slot number back (one byte on the worker's stdin). The worker never writes a slot it has not got back.

use std::io::Read;

use crate::Error;

/// The wire form of an acknowledgement: the slot number as one byte.
#[must_use]
pub fn encode(slot: u32) -> [u8; 1] {
    [u8::try_from(slot).unwrap_or(u8::MAX)]
}

/// The worker's view of which slots it may write.
pub struct SlotGate<const N: usize> {
    free: [bool; N],
}

impl<const N: usize> SlotGate<N> {
    /// All slots start with the worker.
    #[must_use]
    pub fn new() -> Self {
        Self { free: [true; N] }
    }

    /// Hands slot `slot` to the host (the worker has posted it).
    pub fn give(&mut self, slot: usize) {
        self.free[slot] = false;
    }

    /// Blocks reading acknowledgements from `acks` until slot `slot` is the worker's again.
    ///
    /// # Errors
    /// Fails when the host closes the pipe (it is gone) or sends a slot number outside the ring.
    pub fn take(&mut self, slot: usize, acks: &mut impl Read) -> Result<(), Error> {
        while !self.free[slot] {
            let mut byte = [0u8; 1];
            acks.read_exact(&mut byte).map_err(Error::io("waiting for the host to release a slot"))?;
            let acked = usize::from(byte[0]);
            *self
                .free
                .get_mut(acked)
                .ok_or_else(|| Error::Config(format!("the host acknowledged slot {acked} of {N}")))? = true;
        }
        Ok(())
    }
}

impl<const N: usize> Default for SlotGate<N> {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_posted_slot_is_not_reusable_until_acknowledged() {
        let mut gate = SlotGate::<3>::new();
        gate.give(1);
        let mut acks: &[u8] = &[0, 1];
        // The ack for slot 0 is read first and does not free slot 1; the next byte does.
        gate.take(1, &mut acks).unwrap();
        assert_eq!(acks, &[] as &[u8]);
    }

    #[test]
    fn a_closed_pipe_is_an_error_not_a_free_slot() {
        let mut gate = SlotGate::<3>::new();
        gate.give(2);
        assert!(gate.take(2, &mut std::io::empty()).is_err());
    }

    #[test]
    fn an_out_of_range_ack_is_an_error() {
        let mut gate = SlotGate::<3>::new();
        gate.give(0);
        let mut acks: &[u8] = &[9];
        assert!(gate.take(0, &mut acks).is_err());
    }
}
