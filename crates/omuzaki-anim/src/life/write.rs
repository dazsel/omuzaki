//! The one place the bus is written: it enforces "never fight a human" and "amount scales every deviation".

use omuzaki_anim_api::{Channel, ChannelBuf, ChannelMask};

/// Writes processed values onto the bus.
///
/// Every write goes through [`Out::put`] or [`Out::nudge`], so the rules of the contract hold however a behaviour is
/// written: a channel a person drives is never touched, the result is `incoming + amount * (processed - incoming)`,
/// and a value never leaves its channel's legal range.
pub(super) struct Out<'a> {
    buf: &'a mut ChannelBuf,
    /// The incoming values (sanitised), which the deviation is measured from.
    src: &'a ChannelBuf,
    taken: ChannelMask,
    amount: f32,
    written: ChannelMask,
}

impl<'a> Out<'a> {
    /// A writer over `buf`. With `amount <= 0` it writes nothing at all (the bus stays exactly as it came in).
    pub fn new(buf: &'a mut ChannelBuf, src: &'a ChannelBuf, taken: ChannelMask, amount: f32) -> Self {
        Self { buf, src, taken, amount, written: ChannelMask::EMPTY }
    }

    fn open(&self, channel: Channel) -> bool {
        self.amount > 0.0 && !self.taken.contains(channel)
    }

    /// What `channel` holds so far this frame. A non-finite value (an unsanitised incoming channel that nobody drives)
    /// reads as its sanitised incoming value, so a nudge can never turn it into NaN.
    fn current(&self, channel: Channel) -> f32 {
        let value = self.buf[channel];
        if value.is_finite() { value } else { self.src[channel] }
    }

    /// Moves `channel` toward `processed` by `amount`.
    pub fn put(&mut self, channel: Channel, processed: f32) {
        if !self.open(channel) || !processed.is_finite() {
            return;
        }
        let incoming = self.src[channel];
        let (lo, hi) = channel.range();
        self.buf[channel] = (incoming + self.amount * (processed - incoming)).clamp(lo, hi);
        self.written.insert(channel);
    }

    /// Adds a signed `delta` (scaled by `amount`) to the net value `pos - neg` of an opposed pair of channels
    /// (smile against frown, brow up against brow down, eye looking out against in), and keeps both non-negative.
    /// A negative delta first cancels what `pos` holds, then builds `neg`, so the pair's net always moves by the
    /// delta instead of half of it being clipped away at zero.
    pub fn nudge(&mut self, pos: Channel, neg: Channel, delta: f32) {
        if !self.open(pos) || !self.open(neg) || !delta.is_finite() {
            return;
        }
        let delta = self.amount * delta;
        let (mut p, mut n) = (self.current(pos), self.current(neg));
        if delta >= 0.0 {
            let cancelled = delta.min(n);
            n -= cancelled;
            p += delta - cancelled;
        } else {
            let cancelled = (-delta).min(p);
            p -= cancelled;
            n += -delta - cancelled;
        }
        let ((p_lo, p_hi), (n_lo, n_hi)) = (pos.range(), neg.range());
        self.buf[pos] = p.clamp(p_lo, p_hi);
        self.buf[neg] = n.clamp(n_lo, n_hi);
        self.written.insert(pos);
        self.written.insert(neg);
    }

    /// The channels whose value actually differs from what came in (bit for bit): what the layer changed.
    pub fn changed(&self, before: &ChannelBuf) -> ChannelMask {
        self.written.into_iter().filter(|&c| self.buf[c].to_bits() != before[c].to_bits()).collect()
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp)] // exact values copied through
mod tests {
    use super::*;

    fn out<'a>(buf: &'a mut ChannelBuf, src: &'a ChannelBuf, taken: ChannelMask, amount: f32) -> Out<'a> {
        Out::new(buf, src, taken, amount)
    }

    #[test]
    fn put_blends_by_amount_and_clamps() {
        let mut src = ChannelBuf::NEUTRAL;
        src[Channel::JAW_OPEN] = 0.2;
        let mut buf = src;
        out(&mut buf, &src, ChannelMask::EMPTY, 0.5).put(Channel::JAW_OPEN, 0.6);
        assert!((buf[Channel::JAW_OPEN] - 0.4).abs() < 1e-6);
        out(&mut buf, &src, ChannelMask::EMPTY, 3.0).put(Channel::JAW_OPEN, 0.9);
        assert_eq!(buf[Channel::JAW_OPEN], 1.0, "clamped to the channel's range");
    }

    #[test]
    fn people_and_zero_amount_block_writes() {
        let src = ChannelBuf::NEUTRAL;
        let mut buf = src;
        let taken = ChannelMask::of(Channel::JAW_OPEN);
        let mut o = out(&mut buf, &src, taken, 1.0);
        o.put(Channel::JAW_OPEN, 0.5);
        o.nudge(Channel::MOUTH_SMILE_LEFT, Channel::JAW_OPEN, 0.5);
        assert_eq!(buf, src);
        let mut o = out(&mut buf, &src, ChannelMask::EMPTY, 0.0);
        o.put(Channel::BLUSH, 1.0);
        let written = o.written;
        assert_eq!((buf, written), (src, ChannelMask::EMPTY));
    }

    #[test]
    fn nudge_moves_the_net_of_a_pair_exactly() {
        let src = ChannelBuf::NEUTRAL;
        let (up, down) = (Channel::BROW_OUTER_UP_LEFT, Channel::BROW_DOWN_LEFT);
        let net = |b: &ChannelBuf| b[up] - b[down];
        let mut buf = src;
        for delta in [0.1, 0.05, -0.2, -0.05, 0.3] {
            let before = net(&buf);
            out(&mut buf, &src, ChannelMask::EMPTY, 1.0).nudge(up, down, delta);
            assert!((net(&buf) - before - delta).abs() < 1e-6, "delta {delta}");
            assert!(buf[up] == 0.0 || buf[down] == 0.0, "a nudge never raises both channels of a pair");
        }
    }

    #[test]
    fn changed_lists_only_channels_that_differ() {
        let src = ChannelBuf::NEUTRAL;
        let mut buf = src;
        let mut o = out(&mut buf, &src, ChannelMask::EMPTY, 1.0);
        o.put(Channel::BLUSH, 0.0);
        o.put(Channel::JAW_OPEN, 0.3);
        assert_eq!(o.changed(&src), ChannelMask::of(Channel::JAW_OPEN));
    }
}
