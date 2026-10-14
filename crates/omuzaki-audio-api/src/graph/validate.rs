//! Checking a [`GraphSpec`] before it is compiled: unique ids, known references, no cycles, sane gains, and nodes whose
//! kinds exist and fit.

use std::collections::{BTreeMap, BTreeSet};

use super::{BusId, BusSpec, DuckRule, DuckTrigger, GraphSpec, MAX_GAIN_DB, NodeId, NodeKindId};
use crate::node::{NodeKind, NodeKindInfo, node_kind};

/// Looks up what a node kind says about itself. [`GraphSpec::validate_registered`] uses the global registry; tests and
/// tools pass their own.
pub type KindLookup<'a> = &'a dyn Fn(&NodeKindId) -> Option<NodeKindInfo>;

/// Why a graph is not valid. Every variant names what to fix.
#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum GraphError {
    /// Two buses have the same id.
    #[error("bus `{0}` appears twice")]
    DuplicateBus(BusId),
    /// Two nodes in one bus have the same id.
    #[error("bus `{bus}` has two nodes called `{node}`")]
    DuplicateNode {
        /// The bus.
        bus: BusId,
        /// The repeated node id.
        node: NodeId,
    },
    /// A send names a bus that does not exist.
    #[error("bus `{bus}` sends to `{to}`, which does not exist")]
    UnknownSendTarget {
        /// The sending bus.
        bus: BusId,
        /// The missing target.
        to: BusId,
    },
    /// The sends form a loop. The buses listed send to each other in this order, and the last sends to the first.
    #[error("the sends form a cycle: {}", .0.iter().map(ToString::to_string).collect::<Vec<_>>().join(" -> "))]
    Cycle(Vec<BusId>),
    /// A duck rule's target does not exist.
    #[error("a duck rule targets `{0}`, which does not exist")]
    UnknownDuckTarget(BusId),
    /// A duck rule's trigger bus does not exist.
    #[error("a duck rule is triggered by `{0}`, which does not exist")]
    UnknownDuckTrigger(BusId),
    /// A node's sidechain names a bus that does not exist.
    #[error("node `{node}` in bus `{bus}` has the sidechain `{sidechain}`, which does not exist")]
    UnknownSidechain {
        /// The bus holding the node.
        bus: BusId,
        /// The node.
        node: NodeId,
        /// The missing bus.
        sidechain: BusId,
    },
    /// A fader or send gain is NaN or above [`MAX_GAIN_DB`].
    #[error("bus `{bus}` has a gain of {db} dB; gains run from -inf to {MAX_GAIN_DB} dB")]
    GainOutOfRange {
        /// The bus.
        bus: BusId,
        /// The offending gain.
        db: f32,
    },
    /// Two duck rules have the same trigger and target.
    #[error("two duck rules have the trigger {trigger:?} and the target `{target}`")]
    DuplicateDuck {
        /// The shared trigger.
        trigger: DuckTrigger,
        /// The shared target.
        target: BusId,
    },
    /// A duck rule number is outside its range (see [`GraphSpec::validate_structure`]).
    #[error("a duck rule targeting `{target}` has {field} = {value}, outside {min} to {max}")]
    DuckOutOfRange {
        /// The rule's target.
        target: BusId,
        /// The field: `depth_db`, `threshold_db`, `attack_ms`, `hold_ms` or `release_ms`.
        field: &'static str,
        /// The offending value.
        value: f32,
        /// The smallest allowed value.
        min: f32,
        /// The largest allowed value.
        max: f32,
    },
    /// A node's kind is not registered.
    #[error("node `{node}` in bus `{bus}` has the unknown kind `{kind}`")]
    UnknownKind {
        /// The bus holding the node.
        bus: BusId,
        /// The node.
        node: NodeId,
        /// The unknown kind.
        kind: NodeKindId,
    },
    /// A node has a sidechain, but its kind does not take one.
    #[error("node `{node}` in bus `{bus}` has a sidechain, but `{kind}` does not take one")]
    SidechainNotSupported {
        /// The bus holding the node.
        bus: BusId,
        /// The node.
        node: NodeId,
        /// Its kind.
        kind: NodeKindId,
    },
    /// A node's kind does not run on the channel count of its bus.
    #[error("node `{node}` ({kind}) cannot run in the {channels:?} bus `{bus}`")]
    UnsupportedChannels {
        /// The bus holding the node.
        bus: BusId,
        /// The node.
        node: NodeId,
        /// Its kind.
        kind: NodeKindId,
        /// The bus's channels.
        channels: super::Channels,
    },
}

/// The ranges of a duck rule's numbers: depth 0 to 60 dB, threshold -80 to 0 dB, attack 1 to 1,000 ms, hold 0 to
/// 5,000 ms, release 1 to 5,000 ms.
const DUCK_RANGES: [(&str, f32, f32); 5] = [
    ("depth_db", 0.0, 60.0),
    ("threshold_db", -80.0, 0.0),
    ("attack_ms", 1.0, 1000.0),
    ("hold_ms", 0.0, 5000.0),
    ("release_ms", 1.0, 5000.0),
];

fn check_duck_ranges(rule: &DuckRule) -> Result<(), GraphError> {
    let values = [rule.depth_db, rule.threshold_db, rule.attack_ms, rule.hold_ms, rule.release_ms];
    for (value, (field, min, max)) in values.into_iter().zip(DUCK_RANGES) {
        // NaN fails both comparisons' negation, so it is rejected too.
        if !(min..=max).contains(&value) {
            return Err(GraphError::DuckOutOfRange { target: rule.target.clone(), field, value, min, max });
        }
    }
    Ok(())
}

fn gain_ok(db: f32) -> bool {
    !db.is_nan() && db <= MAX_GAIN_DB
}

impl GraphSpec {
    /// Checks everything that needs no node registry: unique bus ids, unique node ids within each bus, known send,
    /// duck and sidechain targets, gains in range, duck-rule numbers in range (depth 0 to 60 dB, threshold -80 to 0 dB,
    /// attack 1 to 1,000 ms, hold 0 to 5,000 ms, release 1 to 5,000 ms), `(trigger, target)` unique among the duck
    /// rules, and **no cycles** among the sends. (A sidechain is not an edge: it
    /// reads the previous block, so it can never close a loop.)
    ///
    /// # Errors
    /// The first [`GraphError`] found, in the order the variants are declared.
    pub fn validate_structure(&self) -> Result<(), GraphError> {
        let mut seen = BTreeSet::new();
        for bus in &self.buses {
            if !seen.insert(&bus.id) {
                return Err(GraphError::DuplicateBus(bus.id.clone()));
            }
        }
        for bus in &self.buses {
            Self::validate_bus(bus, &seen)?;
        }
        for duck in &self.ducks {
            if !seen.contains(&duck.target) {
                return Err(GraphError::UnknownDuckTarget(duck.target.clone()));
            }
            if let super::DuckTrigger::Bus { id } = &duck.trigger
                && !seen.contains(id)
            {
                return Err(GraphError::UnknownDuckTrigger(id.clone()));
            }
        }
        let mut rules = BTreeSet::new();
        for duck in &self.ducks {
            check_duck_ranges(duck)?;
            if !rules.insert((&duck.trigger, &duck.target)) {
                return Err(GraphError::DuplicateDuck { trigger: duck.trigger.clone(), target: duck.target.clone() });
            }
        }
        self.bus_order().map(|_| ())
    }

    fn validate_bus(bus: &BusSpec, buses: &BTreeSet<&BusId>) -> Result<(), GraphError> {
        let mut nodes = BTreeSet::new();
        for node in &bus.chain {
            if !nodes.insert(&node.id) {
                return Err(GraphError::DuplicateNode { bus: bus.id.clone(), node: node.id.clone() });
            }
            if let Some(sidechain) = node.sidechain.as_ref().filter(|s| !buses.contains(s)) {
                return Err(GraphError::UnknownSidechain {
                    bus: bus.id.clone(),
                    node: node.id.clone(),
                    sidechain: sidechain.clone(),
                });
            }
        }
        for send in &bus.sends {
            if !buses.contains(&send.to) {
                return Err(GraphError::UnknownSendTarget { bus: bus.id.clone(), to: send.to.clone() });
            }
        }
        let gains = std::iter::once(bus.gain_db).chain(bus.sends.iter().map(|s| s.gain_db));
        match gains.into_iter().find(|&db| !gain_ok(db)) {
            Some(db) => Err(GraphError::GainOutOfRange { bus: bus.id.clone(), db }),
            None => Ok(()),
        }
    }

    /// [`validate_structure`](Self::validate_structure), then checks every node against `kinds`: the kind exists, runs
    /// on its bus's channel count, and has a sidechain only if it takes one. (A kind that takes a sidechain may run
    /// without one: it then sees `None`.)
    ///
    /// # Errors
    /// The first [`GraphError`] found.
    pub fn validate(&self, kinds: KindLookup<'_>) -> Result<(), GraphError> {
        self.validate_structure()?;
        for bus in &self.buses {
            for node in &bus.chain {
                let info = kinds(&node.kind).ok_or_else(|| GraphError::UnknownKind {
                    bus: bus.id.clone(),
                    node: node.id.clone(),
                    kind: node.kind.clone(),
                })?;
                if node.sidechain.is_some() && !info.sidechain {
                    return Err(GraphError::SidechainNotSupported {
                        bus: bus.id.clone(),
                        node: node.id.clone(),
                        kind: node.kind.clone(),
                    });
                }
                if !info.channels.accepts(bus.channels) {
                    return Err(GraphError::UnsupportedChannels {
                        bus: bus.id.clone(),
                        node: node.id.clone(),
                        kind: node.kind.clone(),
                        channels: bus.channels,
                    });
                }
            }
        }
        Ok(())
    }

    /// [`validate`](Self::validate) against the kinds that registered themselves with `register_node_kind!`.
    ///
    /// # Errors
    /// The first [`GraphError`] found.
    pub fn validate_registered(&self) -> Result<(), GraphError> {
        self.validate(&|id| node_kind(id).map(NodeKind::info))
    }

    /// The buses in processing order: every bus comes after the buses that send to it, and ties keep the order of the
    /// spec, so the answer is deterministic.
    ///
    /// # Errors
    /// [`GraphError::UnknownSendTarget`] for a send to a missing bus, and [`GraphError::Cycle`] when the sends loop.
    pub fn bus_order(&self) -> Result<Vec<BusId>, GraphError> {
        let index: BTreeMap<&BusId, usize> = self.buses.iter().enumerate().map(|(i, b)| (&b.id, i)).collect();
        let mut incoming = vec![0usize; self.buses.len()];
        for bus in &self.buses {
            for send in &bus.sends {
                let to = index
                    .get(&send.to)
                    .ok_or_else(|| GraphError::UnknownSendTarget { bus: bus.id.clone(), to: send.to.clone() })?;
                incoming[*to] += 1;
            }
        }
        let mut order = Vec::with_capacity(self.buses.len());
        let mut done = vec![false; self.buses.len()];
        // Kahn's algorithm, always taking the earliest ready bus so the order is stable.
        while let Some(next) = (0..self.buses.len()).find(|&i| !done[i] && incoming[i] == 0) {
            done[next] = true;
            order.push(self.buses[next].id.clone());
            for send in &self.buses[next].sends {
                if let Some(&to) = index.get(&send.to) {
                    incoming[to] -= 1;
                }
            }
        }
        if order.len() == self.buses.len() {
            return Ok(order);
        }
        Err(GraphError::Cycle(self.find_cycle(&index, &done)))
    }

    /// A cycle among the buses Kahn's algorithm could not place (`done` is false for them). Every such bus has an
    /// unplaced sender, so walking senders backwards must repeat a bus, and the repeat closes a cycle.
    fn find_cycle(&self, index: &BTreeMap<&BusId, usize>, done: &[bool]) -> Vec<BusId> {
        let sender_of = |to: usize| {
            self.buses
                .iter()
                .enumerate()
                .find(|(i, b)| !done[*i] && b.sends.iter().any(|s| index.get(&s.to) == Some(&to)))
                .map(|(i, _)| i)
        };
        let Some(start) = (0..self.buses.len()).find(|&i| !done[i]) else { return Vec::new() };
        let mut walk = vec![start];
        while let Some(prev) = sender_of(*walk.last().unwrap_or(&start)) {
            if let Some(at) = walk.iter().position(|&w| w == prev) {
                // `walk` runs against the sends; reverse the loop so it reads in the direction of the sends.
                let mut cycle: Vec<BusId> = walk[at..].iter().map(|&i| self.buses[i].id.clone()).collect();
                cycle.reverse();
                return cycle;
            }
            walk.push(prev);
        }
        Vec::new()
    }
}
