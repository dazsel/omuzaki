//! [`RecordingStage`]: a [`Stage`] that checks intents against fixed inventories and records them, and
//! [`RestPlanner`], the planner that keeps the face at rest.

use std::collections::BTreeMap;
use std::sync::Mutex;

use omuzaki_anim_api::blend::Layer;
use omuzaki_anim_api::frame::Tick;
use omuzaki_core::rng::Rng;
use omuzaki_core::time::Seconds;
use omuzaki_performer_api::intent::Intent;
use omuzaki_performer_api::moves::{BigMovePlanner, MoveContext, StyleKnobs, TalkState};
use omuzaki_performer_api::stage::{Inventory, Stage, StageError};
use omuzaki_stage_api::ids::MemberId;

use crate::voice::lock;

/// An intent the stage took.
#[derive(Clone, Debug, PartialEq)]
pub struct Applied {
    /// On whom.
    pub member: MemberId,
    /// What, as applied.
    pub intent: Intent,
    /// When it is anchored.
    pub at: Seconds,
}

#[derive(Default)]
struct Record {
    applied: Vec<Applied>,
    released: Vec<(MemberId, Seconds)>,
}

/// A [`Stage`] over fixed inventories, one per member. `apply` refuses what [`Inventory::check`] refuses and records
/// the rest with its `at`; `release` drops recorded intents at or after `from` and records the release.
pub struct RecordingStage {
    inventories: BTreeMap<MemberId, Inventory>,
    record: Mutex<Record>,
}

impl RecordingStage {
    /// A stage with `member` and its inventory.
    pub fn new(member: MemberId, inventory: Inventory) -> Self {
        Self { inventories: BTreeMap::from([(member, inventory)]), record: Mutex::new(Record::default()) }
    }

    /// Adds another member.
    #[must_use]
    pub fn with(mut self, member: MemberId, inventory: Inventory) -> Self {
        self.inventories.insert(member, inventory);
        self
    }

    /// The intents applied and not released, in order.
    pub fn applied(&self) -> Vec<Applied> {
        lock(&self.record).applied.clone()
    }

    /// Every release, in order.
    pub fn released(&self) -> Vec<(MemberId, Seconds)> {
        lock(&self.record).released.clone()
    }
}

impl Stage for RecordingStage {
    fn apply(&self, member: &MemberId, intent: &Intent, at: Seconds) -> Result<(), StageError> {
        let inventory = self.inventory(member)?;
        inventory.check(intent).map_err(StageError::Refused)?;
        lock(&self.record).applied.push(Applied { member: member.clone(), intent: intent.clone(), at });
        Ok(())
    }

    fn release(&self, member: &MemberId, from: Seconds) {
        let mut record = lock(&self.record);
        record.applied.retain(|a| &a.member != member || a.at.0 < from.0);
        record.released.push((member.clone(), from));
    }

    fn inventory(&self, member: &MemberId) -> Result<Inventory, StageError> {
        self.inventories.get(member).cloned().ok_or_else(|| StageError::unknown_member(member))
    }
}

/// A [`BigMovePlanner`] that plans nothing: the face stays at rest. For tests of everything around the planner.
#[derive(Clone, Copy, Debug, Default)]
pub struct RestPlanner;

impl BigMovePlanner for RestPlanner {
    fn plan(&mut self, _: Tick, _: &MoveContext, _: &TalkState, _: &StyleKnobs, _: &mut Rng, out: &mut Layer) {
        out.clear();
    }
}
