//! Node constraints (spec v3 §27, EX-11): VRM 1.0's `VRMC_node_constraint`, where one bone's rotation follows another's.
//!
//! The data model is the extension's (<https://github.com/vrm-c/vrm-specification/tree/master/specification/VRMC_node_constraint-1.0>):
//! a *destination* bone (the constrained one), a *source* bone it follows, a kind and a weight. Three kinds exist:
//! roll (copy the twist about one axis, for twist bones), aim (point one axis at the source, for sleeves) and rotation
//! (copy the whole change from rest, for sub-arm bones). The maths lives in `omuzaki-rig`; this module is the data and
//! the checks VRM leaves to the importer.
//!
//! Constraints run after humanoid, look-at and IK, and before spring bones (the order of `VRMC_vrm-1.0`).
//! Conventions this module fixes where VRM leaves them open:
//! - a bone has at most one constraint, so "the constraint that drives a bone" is unambiguous;
//! - a constraint depends on the constraint of its source bone, if there is one; a chain of such dependencies that
//!   comes back to where it started is a cycle.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::skeleton::{BoneId, Skeleton};

/// A coordinate axis of a bone's own space.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub enum Axis {
    /// The bone's +X axis (the character's left in a VRM T-pose).
    X,
    /// The bone's +Y axis.
    Y,
    /// The bone's +Z axis.
    Z,
}

/// A coordinate axis with a direction.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub enum SignedAxis {
    /// +X.
    PositiveX,
    /// -X.
    NegativeX,
    /// +Y.
    PositiveY,
    /// -Y.
    NegativeY,
    /// +Z.
    PositiveZ,
    /// -Z.
    NegativeZ,
}

/// What a constraint does.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub enum ConstraintKind {
    /// Copy the source's twist about `axis` (an axis of the destination's rest space) to the destination.
    Roll {
        /// The destination axis that receives the roll.
        axis: Axis,
    },
    /// Turn the destination by the smallest rotation that points `axis` at the source.
    Aim {
        /// The destination axis that points at the source.
        axis: SignedAxis,
    },
    /// Copy the source's change from rest to the destination, local space to local space.
    Rotation,
}

/// One constraint: `bone` follows `source`.
#[derive(Copy, Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct BoneConstraint {
    /// The destination: the bone whose rotation the constraint writes.
    pub bone: BoneId,
    /// The bone it follows.
    pub source: BoneId,
    /// What it does.
    pub kind: ConstraintKind,
    /// How much of the result applies, `0..=1`: 0 leaves the destination at rest, 1 applies it fully.
    pub weight: f32,
}

/// Why a [`ConstraintSet`] is not usable. Each variant says what to fix.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ConstraintError {
    /// A bone id is not in the skeleton.
    #[error("constraint {index} refers to bone {bone}, but the skeleton has {len} bones")]
    NoSuchBone {
        /// Index of the constraint in the set.
        index: usize,
        /// The id.
        bone: u16,
        /// The skeleton's size.
        len: usize,
    },
    /// A bone is its own source.
    #[error("bone {bone} is constrained to follow itself")]
    SelfSource {
        /// The bone.
        bone: u16,
    },
    /// Two constraints write the same bone.
    #[error("bone {bone} has more than one constraint")]
    Duplicate {
        /// The bone.
        bone: u16,
    },
    /// A weight is outside `0..=1` or not a number.
    #[error("the constraint on bone {bone} has weight {weight}, which is not in 0..=1")]
    BadWeight {
        /// The destination bone.
        bone: u16,
        /// The weight.
        weight: f32,
    },
    /// Constraints drive each other in a loop.
    #[error("constraints on bones {bones:?} drive each other in a loop")]
    Cycle {
        /// The destination bones of the loop, each following the next, the last following the first.
        bones: Vec<u16>,
    },
}

/// The constraints of one character, in any order.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ConstraintSet {
    /// The constraints; [`ConstraintSet::order`] says in what order to run them.
    pub constraints: Vec<BoneConstraint>,
}

impl ConstraintSet {
    /// Checks the set against `skeleton`: every bone exists, no bone follows itself or has two constraints, every
    /// weight is in `0..=1`, and there is no cycle.
    ///
    /// # Errors
    /// The first problem found, as a [`ConstraintError`].
    pub fn validate(&self, skeleton: &Skeleton) -> Result<(), ConstraintError> {
        let len = skeleton.len();
        for (index, c) in self.constraints.iter().enumerate() {
            for bone in [c.bone, c.source] {
                if bone.index() >= len {
                    return Err(ConstraintError::NoSuchBone { index, bone: bone.0, len });
                }
            }
            if c.bone == c.source {
                return Err(ConstraintError::SelfSource { bone: c.bone.0 });
            }
            if !(0.0..=1.0).contains(&c.weight) {
                return Err(ConstraintError::BadWeight { bone: c.bone.0, weight: c.weight });
            }
        }
        self.order().map(|_| ())
    }

    /// Indices into `constraints` so that every constraint comes after the one that drives its source. Constraints
    /// with no dependency between them keep their declaration order.
    ///
    /// # Errors
    /// [`ConstraintError::Duplicate`] if a bone has two constraints, [`ConstraintError::SelfSource`] if one follows
    /// itself, [`ConstraintError::Cycle`] if constraints drive each other in a loop.
    pub fn order(&self) -> Result<Vec<usize>, ConstraintError> {
        let n = self.constraints.len();
        let mut driver_of = std::collections::BTreeMap::new();
        for (i, c) in self.constraints.iter().enumerate() {
            if driver_of.insert(c.bone, i).is_some() {
                return Err(ConstraintError::Duplicate { bone: c.bone.0 });
            }
            if c.bone == c.source {
                return Err(ConstraintError::SelfSource { bone: c.bone.0 });
            }
        }
        // The constraint each one waits for: the one that writes its source bone, if any.
        let dep: Vec<Option<usize>> = self.constraints.iter().map(|c| driver_of.get(&c.source).copied()).collect();

        // 0 = unvisited, 1 = on the current chain, 2 = placed.
        let mut state = vec![0_u8; n];
        let mut out = Vec::with_capacity(n);
        let mut chain: Vec<usize> = Vec::new();
        for start in 0..n {
            let mut cur = Some(start);
            // Walk down the dependency chain until something already placed (or nothing) is reached.
            while let Some(i) = cur {
                match state[i] {
                    2 => break,
                    1 => {
                        let from = chain.iter().position(|&j| j == i).unwrap_or(0);
                        let bones = chain[from..].iter().map(|&j| self.constraints[j].bone.0).collect();
                        return Err(ConstraintError::Cycle { bones });
                    }
                    _ => {
                        state[i] = 1;
                        chain.push(i);
                        cur = dep[i];
                    }
                }
            }
            // The chain ends at a root; dependencies were pushed last, so place them first.
            while let Some(i) = chain.pop() {
                state[i] = 2;
                out.push(i);
            }
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::skeleton::{BoneDef, Transform};

    fn skeleton(n: usize) -> Skeleton {
        let bones = (0..n)
            .map(|i| BoneDef {
                name: format!("b{i}"),
                parent: (i > 0).then(|| BoneId(0)),
                rest: Transform::IDENTITY,
                humanoid: None,
            })
            .collect();
        Skeleton::generic(bones).unwrap_or_else(|e| panic!("fixture skeleton: {e}"))
    }

    fn c(bone: u16, source: u16) -> BoneConstraint {
        BoneConstraint { bone: BoneId(bone), source: BoneId(source), kind: ConstraintKind::Rotation, weight: 1.0 }
    }

    fn set(list: &[(u16, u16)]) -> ConstraintSet {
        ConstraintSet { constraints: list.iter().map(|&(b, s)| c(b, s)).collect() }
    }

    #[test]
    fn dependencies_come_first_whatever_the_declaration_order() {
        // 1 follows 2, 2 follows 3 (unconstrained), 4 follows 1.
        let s = set(&[(4, 1), (1, 2), (2, 3)]);
        assert_eq!(s.order(), Ok(vec![2, 1, 0]));
        assert_eq!(set(&[(1, 5), (2, 6)]).order(), Ok(vec![0, 1]), "independent constraints keep their order");
    }

    #[test]
    fn a_loop_names_its_bones_and_not_the_bystanders() {
        let s = set(&[(3, 1), (1, 2), (2, 1)]);
        assert_eq!(s.order(), Err(ConstraintError::Cycle { bones: vec![1, 2] }));
    }

    #[test]
    fn validate_rejects_each_mistake() {
        let sk = skeleton(5);
        assert_eq!(set(&[(1, 2), (3, 4)]).validate(&sk), Ok(()));
        assert!(matches!(set(&[(1, 9)]).validate(&sk), Err(ConstraintError::NoSuchBone { index: 0, bone: 9, len: 5 })));
        assert_eq!(set(&[(2, 2)]).validate(&sk), Err(ConstraintError::SelfSource { bone: 2 }));
        assert_eq!(set(&[(2, 1), (2, 3)]).validate(&sk), Err(ConstraintError::Duplicate { bone: 2 }));
        for weight in [-0.1, 1.5, f32::NAN] {
            let mut bad = set(&[(1, 2)]);
            bad.constraints[0].weight = weight;
            assert!(matches!(bad.validate(&sk), Err(ConstraintError::BadWeight { bone: 1, .. })));
        }
        assert!(matches!(set(&[(1, 2), (2, 1)]).validate(&sk), Err(ConstraintError::Cycle { .. })));
    }

    #[test]
    fn the_set_round_trips_through_json() {
        let s = ConstraintSet {
            constraints: vec![BoneConstraint {
                bone: BoneId(1),
                source: BoneId(2),
                kind: ConstraintKind::Aim { axis: SignedAxis::NegativeY },
                weight: 0.5,
            }],
        };
        let json = serde_json::to_string(&s).unwrap_or_default();
        assert_eq!(serde_json::from_str::<ConstraintSet>(&json).ok(), Some(s));
    }
}
