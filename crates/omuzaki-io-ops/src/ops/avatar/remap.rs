//! `avatar.remap`: sets or clears the person's choice of node for one humanoid bone of an imported character (design
//! D4, D6).
//!
//! The choice is stored in `characters/<id>/avatar/humanoid` with the node's index and its name (the name lets a
//! re-import of an updated file find the node again). The avatar is rebuilt with it before anything is written: a
//! choice the humanoid rules refuse (the node is not under the bone's parent bone, already plays another bone, or
//! breaks a required bone) is refused with `humanoid.override_invalid` and changes nothing. One undo reverts it.

use omuzaki_anim_api::{ChannelMask, HumanoidBone};
use omuzaki_avatar_api::def::NodeChoice;
use omuzaki_char_api::CharId;
use omuzaki_core::text::nearest;
use omuzaki_io_api::ImportOptions;
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use omuzaki_render_api::Capabilities;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::{def_patch, mtoon, prepare_imported, read, read_def, require_clean, stored};

/// Sets or clears which node plays one humanoid bone of an imported character.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend(
    "examples" = [{ "character": "avatar", "bone": "leftHand", "node": 8 }],
    "x-omuzaki-fixture" = "avatar"
))]
pub struct Remap {
    /// The imported character.
    pub character: CharId,
    /// The humanoid bone, by its VRM 1.0 name (`leftHand`).
    pub bone: String,
    /// The node that plays it, by its index in the file; `null` clears the choice, so the file's own map (or the
    /// automatic one) decides again.
    #[serde(default)]
    pub node: Option<u32>,
}

impl Op for Remap {
    fn meta() -> OpMeta {
        OpMeta {
            name: "avatar.remap",
            summary: "Set or clear which node of an imported character's model plays one humanoid bone.",
            kind: OpKind::Edit,
            scope: Scope::EditCharacter,
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let mut def = read_def(ctx.doc, &self.character)?;
        let bone = HumanoidBone::from_name(&self.bone).ok_or_else(|| {
            let names = HumanoidBone::ALL.map(HumanoidBone::name);
            let error = OpError::invalid("bone", format!("{:?} is not a VRM humanoid bone", self.bone));
            match nearest(&self.bone, names) {
                Some(near) => error.hint(format!("did you mean {near}?")),
                None => error.hint("use a VRM 1.0 bone name such as leftHand"),
            }
        })?;
        let original = stored(ctx, &def)?;
        let source = original.source();
        let options = ImportOptions::default();
        let read = read(&source, &options, ctx.clock)?;
        match self.node {
            Some(node) => {
                let named = usize::try_from(node).ok().and_then(|i| read.outcome.scene.nodes.get(i));
                let Some(named) = named else {
                    let count = read.outcome.scene.nodes.len();
                    return Err(OpError::invalid("node", format!("the model has {count} nodes; there is no node {node}")));
                };
                def.humanoid.insert(bone, NodeChoice::new(node, named.name.clone()));
            }
            None => {
                def.humanoid.remove(&bone);
            }
        }
        let mtoon = mtoon(ctx.service::<Capabilities>().ok());
        let prepared = prepare_imported(read, &source, Some(def.clone()), &options, mtoon, ctx.clock, ctx.doc)
            .map_err(|e| OpError { field: Some("node".to_owned()), ..e })?;
        require_clean(&prepared.report, "node")?;
        let driven: ChannelMask = prepared.avatar.driven();
        let output = json!({ "character": self.character, "bone": bone.name(), "driven": driven });
        Ok(Plan { output: Some(output), ..Plan::edit(def_patch(ctx.doc, &self.character, &def)?) })
    }
}

register_op!(Remap);
