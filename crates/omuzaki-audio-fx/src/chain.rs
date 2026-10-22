//! A mono effect chain run outside the engine: what the budget gates, the CPU bench and the `audio.fx.render` job use
//! to put audio through a preset with the same nodes the engine would build.
//!
//! It follows the engine's rules for a chain (`omuzaki-audio-engine`'s mixer core): nodes run in order, each starts at
//! its parameter defaults and then takes the values its [`NodeSpec`] sets, and a disabled node only delays its input
//! by its latency, so switching it off never shifts the bus in time.

use omuzaki_audio_api::graph::{Channels, NodeSpec};
use omuzaki_audio_api::node::{BlockMut, MAX_BLOCK, Node, node_kind};

/// One node of a [`Chain`].
struct Link {
    node: Box<dyn Node>,
    enabled: bool,
    /// A disabled node's delay line (its latency plus one block), and where it writes next.
    hold: Vec<f32>,
    at: usize,
}

/// A built mono chain.
pub struct Chain {
    links: Vec<Link>,
    next: u64,
}

impl std::fmt::Debug for Chain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Chain").field("nodes", &self.links.len()).field("latency", &self.latency()).finish()
    }
}

/// Why a chain can't be built.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
pub enum ChainError {
    /// A node names a kind this build doesn't have.
    #[error("node `{node}`: there is no effect kind `{kind}` in this build")]
    UnknownKind {
        /// The node.
        node: String,
        /// Its kind.
        kind: String,
    },
    /// A node sets a parameter its kind doesn't have, or a value outside its range.
    #[error("node `{node}`: {message}")]
    BadParam {
        /// The node.
        node: String,
        /// What is wrong.
        message: String,
    },
    /// The kind refused the node's config or a mono bus.
    #[error("node `{node}`: {message}")]
    Refused {
        /// The node.
        node: String,
        /// The kind's reason.
        message: String,
    },
    /// Two nodes share an id.
    #[error("two nodes are called `{0}`; node ids are unique within a chain")]
    DuplicateId(String),
}

/// Checks `chain` against the registered kinds without building it: every kind exists, every parameter is the kind's
/// and in its range, and the ids are unique.
///
/// # Errors
/// The first [`ChainError`] found.
pub fn check(chain: &[NodeSpec]) -> Result<(), ChainError> {
    for (i, spec) in chain.iter().enumerate() {
        if chain[..i].iter().any(|other| other.id == spec.id) {
            return Err(ChainError::DuplicateId(spec.id.to_string()));
        }
        let kind = node_kind(&spec.kind)
            .ok_or_else(|| ChainError::UnknownKind { node: spec.id.to_string(), kind: spec.kind.to_string() })?;
        let info = kind.info();
        for (name, value) in &spec.params {
            let Some(param) = info.params.iter().find(|p| p.name == name) else {
                let known = info.params.iter().map(|p| p.name).collect::<Vec<_>>().join(", ");
                return Err(ChainError::BadParam {
                    node: spec.id.to_string(),
                    message: format!("`{}` has no parameter `{name}` (it has {known})", info.id),
                });
            };
            if !value.is_finite() || *value < param.min || *value > param.max {
                return Err(ChainError::BadParam {
                    node: spec.id.to_string(),
                    message: format!("`{name}` is {value}, outside {} to {}", param.min, param.max),
                });
            }
        }
    }
    Ok(())
}

impl Chain {
    /// Builds every node of `chain` for a mono bus, with its parameters set.
    ///
    /// # Errors
    /// What [`check`] finds, or [`ChainError::Refused`] when a kind refuses its config.
    pub fn build(chain: &[NodeSpec]) -> Result<Self, ChainError> {
        check(chain)?;
        let mut links = Vec::with_capacity(chain.len());
        for spec in chain {
            let refused = |message: String| ChainError::Refused { node: spec.id.to_string(), message };
            let kind = node_kind(&spec.kind)
                .ok_or_else(|| ChainError::UnknownKind { node: spec.id.to_string(), kind: spec.kind.to_string() })?;
            let mut node = kind.create(&spec.config, Channels::Mono).map_err(|e| refused(e.to_string()))?;
            let info = kind.info();
            for (name, value) in &spec.params {
                if let Some(index) = info.param_index(name) {
                    node.set_param(index, *value);
                }
            }
            node.reset();
            let hold = if spec.enabled { Vec::new() } else { vec![0.0; node.latency() as usize + MAX_BLOCK] };
            links.push(Link { node, enabled: spec.enabled, hold, at: 0 });
        }
        Ok(Self { links, next: 0 })
    }

    /// The chain's declared latency in samples at 48 kHz: the sum of its nodes', enabled or not.
    #[must_use]
    pub fn latency(&self) -> u32 {
        self.links.iter().map(|l| l.node.latency()).sum()
    }

    /// Runs `samples` through the chain in place, in blocks of at most [`MAX_BLOCK`].
    pub fn process(&mut self, samples: &mut [f32]) {
        for block in samples.chunks_mut(MAX_BLOCK) {
            for link in &mut self.links {
                if link.enabled {
                    link.node.process(&mut BlockMut::mono(block, self.next));
                } else {
                    delay(link, block);
                }
            }
            self.next += block.len() as u64;
        }
    }

    /// The summed gain reduction the nodes report, in dB (positive).
    #[must_use]
    pub fn gain_reduction_db(&self) -> f32 {
        self.links.iter().filter(|l| l.enabled).map(|l| l.node.report().gain_reduction_db).sum()
    }
}

/// A disabled node's pass: the input, `latency` samples late.
fn delay(link: &mut Link, block: &mut [f32]) {
    let latency = link.node.latency() as usize;
    if latency == 0 {
        return;
    }
    let len = link.hold.len();
    for s in block.iter_mut() {
        link.hold[link.at] = *s;
        *s = link.hold[(link.at + len - latency) % len];
        link.at = (link.at + 1) % len;
    }
}
