//! `face.channel.sweep` (FE-04, FE-01, VF-02): paints a character's face with each face channel pushed to its far end
//! and reports which channels visibly change it and which are dead controls, which move it the wrong way or on the wrong
//! side (the expected-effect table, [`crate::preview::direction`]), with a contact sheet of every push.
//!
//! It is a job: `plan` checks the arguments and that the face reads, `work` paints off the document lock (about two
//! seconds for every channel at the default width on one core) and polls for a cancel between channels. The measure is
//! [`crate::preview::sweep`]'s; the sheet goes to the asset store when the app has one.

use omuzaki_anim_api::Channel;
use omuzaki_char_api::CharId;
use omuzaki_ops_api::{JobCtx, Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::ops_shared::sheet::{SHEET_CELL_PX, store_sheet};
use crate::ops_shared::subject::subject;
use crate::preview::direction::Direction;
use crate::preview::metrics::downsample;
use crate::preview::sheet::{Cell, compose};
use crate::preview::sweep::{ChannelStatus, SweepOptions, default_min_changed_px, face_channels, sweep};
use crate::preview::{MAX_WIDTH_PX, MIN_WIDTH_PX};

/// The default picture width: half the reference width, fast and still sharp enough to see a lash move.
const DEFAULT_WIDTH_PX: u32 = 774;
/// Contact sheet columns.
const SHEET_COLUMNS: u32 = 10;

/// Pushes each face channel of a character to its far end and reports which ones visibly change the face.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{"character": "kit", "width_px": 387}], "x-omuzaki-fixture" = "character"))]
pub struct SweepChannels {
    /// The character whose face to sweep.
    pub character: CharId,
    /// The channels to sweep, by name (`jawOpen`, `eyeBlinkLeft`, `sparkle`); every face channel when omitted.
    #[serde(default)]
    pub channels: Option<Vec<String>>,
    /// The picture width in pixels (8 to 4096).
    #[serde(default = "default_width")]
    pub width_px: u32,
    /// A channel is dead when no layer changes in more pixels than this; scaled from 40 at 1548 px when omitted.
    #[serde(default)]
    pub min_changed_px: Option<u32>,
    /// Store a contact sheet of every push (needs the app's asset store).
    #[serde(default = "yes")]
    pub sheet: bool,
}

fn default_width() -> u32 {
    DEFAULT_WIDTH_PX
}

fn yes() -> bool {
    true
}

impl SweepChannels {
    /// The channels asked for, in order, or every face channel.
    fn channels(&self) -> Result<Vec<Channel>, OpError> {
        let Some(names) = &self.channels else { return Ok(face_channels()) };
        if names.is_empty() {
            return Err(OpError::invalid("channels", "the list is empty").hint("omit it to sweep every face channel"));
        }
        names
            .iter()
            .enumerate()
            .map(|(i, name)| {
                Channel::from_name(name).ok_or_else(|| {
                    OpError::invalid(format!("channels/{i}"), format!("there is no channel '{name}'"))
                        .hint("channel names are ARKit's (jawOpen, eyeBlinkLeft) and the anime extras (eyeArcLeft, sparkle)")
                })
            })
            .collect()
    }

    fn options(&self) -> Result<SweepOptions, OpError> {
        if !(MIN_WIDTH_PX..=MAX_WIDTH_PX).contains(&self.width_px) {
            return Err(OpError::invalid(
                "width_px",
                format!("width_px = {} is out of range: use {MIN_WIDTH_PX} to {MAX_WIDTH_PX}", self.width_px),
            ));
        }
        Ok(SweepOptions {
            channels: self.channels()?,
            width_px: self.width_px,
            min_changed_px: self.min_changed_px.unwrap_or_else(|| default_min_changed_px(self.width_px)),
            keep_pictures: self.sheet,
        })
    }
}

impl Op for SweepChannels {
    fn meta() -> OpMeta {
        OpMeta {
            name: "face.channel.sweep",
            summary: "Push each face channel of a character to its far end and report which ones visibly change the \
                      face (dead controls) and which move it the wrong way or on the wrong side, with a contact sheet",
            kind: OpKind::Job,
            scope: Scope::Render,
            // It changes nothing in the document (its patch is empty); its sheets go to the content-addressed store.
            undo: Undo::Auto,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let options = self.options()?;
        subject(ctx.doc, &self.character)?;
        Ok(Plan::answer(json!({
            "character": self.character.to_string(),
            "channels": options.channels.len(),
            "width_px": options.width_px,
            "min_changed_px": options.min_changed_px,
            "dry_run": ctx.dry_run,
        })))
    }

    fn work(&self, job: &JobCtx<'_>) -> Result<Value, OpError> {
        job.checkpoint("reading the face", 0.0)?;
        let options = self.options()?;
        let subject = job.read(|doc| subject(doc, &self.character))?;
        job.checkpoint("painting", 0.05)?;
        let report = sweep(&subject, &options, &|| job.is_cancelled())
            .map_err(|e| OpError::internal(format!("painting the face failed: {e}")))?
            .ok_or_else(|| OpError::cancelled(job.id()))?;
        let mut answer = json!({
            "character": self.character.to_string(),
            "width_px": options.width_px,
            "min_changed_px": options.min_changed_px,
            "summary": {
                "moves": report.with_status(ChannelStatus::Moves).len(),
                "dead": report.with_status(ChannelStatus::Dead),
                "unmapped": report.with_status(ChannelStatus::Unmapped),
                "off_in_design": report.with_status(ChannelStatus::OffInDesign),
                "wrong_way": report.with_direction(Direction::WrongWay),
                "wrong_side": report.with_direction(Direction::WrongSide),
                "no_expectation": report.with_direction(Direction::Missing),
            },
            "rows": report.rows,
        });
        if self.sheet {
            job.checkpoint("making the contact sheet", 0.9)?;
            let mut cells = Vec::with_capacity(report.rows.len() + 1);
            if let Some(rest) = &report.rest {
                cells.push(Cell { picture: downsample(rest, SHEET_CELL_PX), label: "rest".to_owned(), flagged: false });
            }
            for row in &report.rows {
                if let Some(picture) = &row.picture {
                    let flagged = row.status == ChannelStatus::Dead
                        || matches!(row.direction, Direction::WrongWay | Direction::WrongSide);
                    cells.push(Cell { picture: downsample(picture, SHEET_CELL_PX), label: row.channel.to_owned(), flagged });
                }
            }
            answer["sheet"] = store_sheet(job, &compose(&cells, SHEET_COLUMNS))?;
        }
        Ok(answer)
    }
}

register_op!(SweepChannels);
