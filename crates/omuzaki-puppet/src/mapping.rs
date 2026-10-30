//! Driving a puppet from the channel bus: a [`ParamMapping`] resolved against a puppet and the channel table once,
//! then applied every frame without allocating.

use omuzaki_anim_api::{Channel, ChannelBuf};
use omuzaki_puppet_api::{MappingEntry, ParamIx, ParamMapping, PuppetDef};

use crate::eval::Evaluator;

#[derive(Clone, Debug)]
struct Resolved {
    param: ParamIx,
    axis: usize,
    channel: Channel,
    minus: Option<Channel>,
    entry: MappingEntry,
}

/// A mapping bound to one puppet.
#[derive(Clone, Debug)]
pub struct Driver {
    entries: Vec<Resolved>,
    /// Per parameter: which axes the mapping drives.
    driven: Vec<[bool; 2]>,
    sums: Vec<[f32; 2]>,
    /// Entries that named a parameter or channel that doesn't exist, as `param <- channel`.
    pub unresolved: Vec<String>,
}

impl Driver {
    /// Resolves `mapping` against `def`; entries naming a missing parameter or an unknown channel are listed in
    /// [`Driver::unresolved`] and skipped.
    #[must_use]
    pub fn new(def: &PuppetDef, mapping: &ParamMapping) -> Self {
        let mut entries = Vec::new();
        let mut unresolved = Vec::new();
        let mut driven = vec![[false; 2]; def.params.len()];
        for e in &mapping.entries {
            let param = def.param(&e.param);
            let channel = Channel::from_name(&e.source.channel);
            let minus = e.source.minus.as_deref().map(Channel::from_name);
            match (param, channel, minus) {
                (Some(param), Some(channel), None | Some(Some(_))) if e.axis < 2 => {
                    let axis = usize::from(e.axis);
                    driven[param.get()][axis] = true;
                    entries.push(Resolved { param, axis, channel, minus: minus.flatten(), entry: e.clone() });
                }
                _ => unresolved.push(format!("{} <- {}", e.param, e.source.channel)),
            }
        }
        let sums = vec![[0.0; 2]; def.params.len()];
        Self { entries, driven, sums, unresolved }
    }

    /// Sets every driven parameter of `eval` from `channels`: the sum of its entries. Parameters the mapping doesn't
    /// drive keep their values (physics, scripts). Allocates nothing.
    pub fn apply(&mut self, channels: &ChannelBuf, eval: &mut Evaluator) {
        self.sums.iter_mut().for_each(|s| *s = [0.0; 2]);
        for r in &self.entries {
            let v = channels.get(r.channel) - r.minus.map_or(0.0, |m| channels.get(m));
            self.sums[r.param.get()][r.axis] += r.entry.map(v);
        }
        for (i, (sum, driven)) in self.sums.iter().zip(&self.driven).enumerate() {
            if !(driven[0] || driven[1]) {
                continue;
            }
            let param = ParamIx(u32::try_from(i).unwrap_or(u32::MAX));
            let current = eval.value(param);
            let x = if driven[0] { sum[0] } else { current[0] };
            let y = if driven[1] { sum[1] } else { current[1] };
            eval.set(param, [x, y]);
        }
    }
}
