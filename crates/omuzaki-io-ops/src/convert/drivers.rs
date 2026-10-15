//! Bone drivers for the VRM avatar block: the character's [`BoneDriver`]s as `VRMC_node_constraint`s.
//!
//! [`omuzaki_char_api::export_plan`] decides what each driver becomes; this maps that onto the file's types:
//! - **A `Follow` driver is a node constraint**: a rotation constraint (whole rotation) or a roll constraint (the
//!   twist about one axis), with its source node and weight. Its maths is the spec's, so a VRM viewer moves the bone as
//!   the stage does, with no baking.
//! - **A `Channel` driver is left out, with a warning.** It needs a baked animation clip, and the scene has no
//!   animation type yet; the driven bone keeps its skeleton rest in the file.
//!
//! Bone `i` is node `i` in a gathered scene ([`bone_node`]), so no lookup table is needed. Weights are shares, not
//! lengths, so a scene's scale leaves them alone.

use omuzaki_char_api::drivers::{self, BoneDriver, DriverExport, RollAxis};
use omuzaki_io_api::{self as io, ConstraintKind, Finding, NodeConstraint, Report};

use crate::gather::skeleton::bone_node;

/// The node constraints of `drivers`, in the drivers' order.
///
/// `drivers` must be a built character's, validated against its skeleton (`CharacterRig::drivers`), so every bone is
/// a node. A `Channel` driver has no constraint form; it is reported in `report` as a warning naming the bone and the
/// channel, never dropped silently. Deterministic: the same drivers give the same constraints and findings.
pub fn to_io(drivers: &[BoneDriver], report: &mut Report) -> Vec<NodeConstraint> {
    let mut out = Vec::new();
    for export in drivers::export_plan(drivers) {
        match export {
            DriverExport::Constraint(c) => {
                let source = bone_node(c.source);
                let kind = match c.axis {
                    None => ConstraintKind::Rotation { source },
                    Some(axis) => ConstraintKind::Roll { source, axis: roll_axis(axis) },
                };
                out.push(NodeConstraint { node: bone_node(c.bone), kind, weight: c.weight });
            }
            DriverExport::Baked(baked) => {
                let msg = format!(
                    "bone {} is driven by the channel {}, which the file cannot carry yet",
                    baked.bone.0,
                    baked.channel.name()
                );
                let hint = "the bone keeps its rest pose in the file: a viewer cannot play the channel";
                report.push(Finding::warning(RULE_CHANNEL_DRIVER, msg).at("avatar/constraints").hint(hint));
            }
        }
    }
    out
}

/// The report rule for a `Channel` driver left out of the file.
pub const RULE_CHANNEL_DRIVER: &str = "gather.channel_driver_not_exported";

fn roll_axis(axis: RollAxis) -> io::RollAxis {
    match axis {
        RollAxis::X => io::RollAxis::X,
        RollAxis::Y => io::RollAxis::Y,
        RollAxis::Z => io::RollAxis::Z,
    }
}

#[cfg(test)]
mod tests {
    use omuzaki_anim_api::{BoneId, Channel, Transform};
    use omuzaki_char_api::drivers::{Drive, Mapping};
    use omuzaki_core::glam::Quat;
    use omuzaki_io_api::NodeIx;

    use super::*;

    fn roll(bone: u16, source: u16, axis: RollAxis, weight: f32) -> BoneDriver {
        BoneDriver { bone: BoneId(bone), drive: Drive::Follow { source: BoneId(source), axis: Some(axis), weight } }
    }

    fn fold(bone: u16) -> BoneDriver {
        let to = Transform { rotation: Quat::from_rotation_z(1.2), ..Transform::IDENTITY };
        BoneDriver {
            bone: BoneId(bone),
            drive: Drive::Channel {
                channel: Channel::custom(3).unwrap_or_else(|| panic!("custom slot 3 exists")),
                from: Transform::IDENTITY,
                to,
                mapping: Mapping::Clamped,
            },
        }
    }

    #[test]
    fn follow_drivers_become_rotation_and_roll_constraints_on_their_nodes() {
        let mut report = Report::default();
        let drivers = [BoneDriver::follow(BoneId(60), BoneId(14), 0.5), roll(61, 15, RollAxis::Y, 0.25)];
        let constraints = to_io(&drivers, &mut report);
        assert_eq!(
            constraints,
            [
                NodeConstraint { node: NodeIx(60), kind: ConstraintKind::Rotation { source: NodeIx(14) }, weight: 0.5 },
                NodeConstraint {
                    node: NodeIx(61),
                    kind: ConstraintKind::Roll { source: NodeIx(15), axis: io::RollAxis::Y },
                    weight: 0.25
                },
            ]
        );
        assert_eq!(report.findings, []);
    }

    #[test]
    fn every_roll_axis_maps_to_its_twin() {
        for (ours, theirs) in
            [(RollAxis::X, io::RollAxis::X), (RollAxis::Y, io::RollAxis::Y), (RollAxis::Z, io::RollAxis::Z)]
        {
            let constraints = to_io(&[roll(2, 1, ours, 1.0)], &mut Report::default());
            assert_eq!(constraints[0].kind, ConstraintKind::Roll { source: NodeIx(1), axis: theirs });
        }
    }

    #[test]
    fn a_channel_driver_is_left_out_with_a_warning_naming_the_bone_and_the_channel() {
        let mut report = Report::default();
        let drivers = [fold(70), BoneDriver::follow(BoneId(71), BoneId(70), 1.0)];
        let constraints = to_io(&drivers, &mut report);
        assert_eq!(constraints.len(), 1, "the follow driver still exports");
        assert_eq!(constraints[0].node, NodeIx(71));
        let [finding] = &report.findings[..] else { panic!("one finding, got {:?}", report.findings) };
        assert_eq!(finding.rule, RULE_CHANNEL_DRIVER);
        assert!(finding.message.contains("bone 70") && finding.message.contains("custom"), "{}", finding.message);
    }

    #[test]
    fn no_drivers_no_constraints_and_the_mapping_is_deterministic() {
        assert_eq!(to_io(&[], &mut Report::default()), []);
        let drivers = [fold(5), roll(6, 2, RollAxis::Z, 0.7), BoneDriver::follow(BoneId(7), BoneId(6), 0.1)];
        let (mut a, mut b) = (Report::default(), Report::default());
        assert_eq!(to_io(&drivers, &mut a), to_io(&drivers, &mut b));
        assert_eq!(a, b);
    }
}
