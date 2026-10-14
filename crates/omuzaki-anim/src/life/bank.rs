//! A family of springs, one per channel, sharing one feel.

use omuzaki_anim_api::{Channel, Dynamics};

use super::frame::{Frame, Side};
use super::spring::Spring;
use super::write::Out;

/// The springs of one channel family (the eye lids, the brows, the mouth). Each channel chases its own target at the
/// family's frequency, scaled by its side so that the leading side of the face arrives first.
#[derive(Clone, Debug)]
pub(super) struct Bank<const N: usize> {
    slots: [(Channel, Side); N],
    springs: [Spring; N],
}

impl<const N: usize> Bank<N> {
    pub fn new(slots: [(Channel, Side); N]) -> Self {
        Self { slots, springs: [Spring::default(); N] }
    }

    /// Advances every spring one frame toward `target(index, channel)` and returns the new positions.
    /// With `follow` false (a person drives the family) the springs snap to the targets instead.
    pub fn track(
        &mut self,
        frame: &Frame<'_>,
        dynamics: Dynamics,
        follow: bool,
        target: impl Fn(usize, Channel) -> f32,
    ) -> [f32; N] {
        let mut positions = [0.0; N];
        for (i, ((channel, side), spring)) in self.slots.iter().zip(&mut self.springs).enumerate() {
            let scale = frame.freq_scale(*side);
            positions[i] = spring.track(target(i, *channel), follow, dynamics, scale, frame.sub);
        }
        positions
    }

    /// Writes the positions returned by [`Bank::track`].
    pub fn write(&self, out: &mut Out<'_>, positions: &[f32; N]) {
        for ((channel, _), &position) in self.slots.iter().zip(positions) {
            out.put(*channel, position);
        }
    }
}
