//! `io.import {as: avatar}` (design D5, D6): the import the confirm screen confirms.
//!
//! The job prepares the file again exactly as `io.inspect {as: avatar}` did and recomputes the confirm token: a
//! different token (the file, a file it refers to, or an option changed since the person looked) refuses the import
//! with [`CONFIRM_STALE`], even when the findings would read the same. A model whose terms let only its author perform
//! as it (`rights.avatar_permission`) needs `acknowledge: true` from a person, recorded in the node with the date and
//! who; without it the import is refused with [`ACKNOWLEDGEMENT_NEEDED`]. No other rights finding blocks anything:
//! performing on your own machine is not publishing.
//!
//! Then it stores the original and its images (the reference import's storing, with the same rights rules) and, in
//! one patch `finish` returns, writes `characters/<id>/name`, `characters/<id>/avatar` (the [`AvatarDef`]) and the
//! source row `characters/<id>/sources/avatar` (the file's provenance, and its own terms), and records the asset. One
//! undo removes all of it; a cancel before `finish` writes nothing.
//!
//! The two refusal codes lead the error message (`io.confirm_stale: …`), with `field` naming the argument at fault:
//! the op error has no code field of its own beyond its class.

use omuzaki_avatar_api::def::{Acknowledgement, AvatarDef};
use omuzaki_avatar_api::summary::{AvatarSummary, Stage};
use omuzaki_char_api::{CharId, PartId, paths};
use omuzaki_core::time::{Clock, Seconds};
use omuzaki_history_api::WallClock;
use omuzaki_io_api::{ImportOptions, Report, SceneSummary};
use omuzaki_ops_api::{Actor, JobCtx, OpCtx, OpError, Plan};
use omuzaki_render_api::Capabilities;
use omuzaki_store_api::{Provenance, Store};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{Source, mtoon, prepare, timed};
use crate::ops::import::{AssetRecord, FolderFiles, Import, JobProgress, record_patch, store_files, strip_nulls};
use crate::ops::licence::source::{PartSource, row_patch};

/// The refusal of an import whose confirm token is not the one the file and options give now.
pub const CONFIRM_STALE: &str = "io.confirm_stale";

/// The refusal of an import that needs the person's acknowledgement of the model's terms and lacks it.
pub const ACKNOWLEDGEMENT_NEEDED: &str = "io.acknowledgement_needed";

/// The reserved part id of an imported avatar's own source row (`docs/design/byom.md` D5).
pub const AVATAR_PART: &str = "avatar";

/// The plan output key that carries who acknowledged the terms to the job.
const ACKNOWLEDGED_BY: &str = "acknowledged_by";

/// What an avatar import produced, between `work` and `finish`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub(crate) struct AvatarImported {
    /// The stored original, as the document records it.
    pub asset: AssetRecord,
    /// The character it becomes.
    pub id: CharId,
    /// The character's name: the model's own, or the file's.
    pub name: String,
    /// The avatar node written at `characters/<id>/avatar`.
    pub def: AvatarDef,
    /// The source row written at `characters/<id>/sources/avatar`.
    pub row: PartSource,
    /// The prepared avatar at a glance, with the store's timing.
    pub summary: AvatarSummary,
    /// Counts of what the file holds.
    pub scene: SceneSummary,
    /// Every finding.
    pub report: Report,
    /// The record the original was stored with.
    pub provenance: Provenance,
}

/// A clock that always reads zero, for a host that offers none: every stage then reports no time.
struct Unclocked;

impl Clock for Unclocked {
    fn now(&self) -> Seconds {
        Seconds(0.0)
    }
}

/// The plan of an avatar import: the token is given, the id (if named) is free, and an acknowledgement comes from a
/// person, whose name the job records.
///
/// # Errors
/// `Invalid` without a token or with a character (an avatar becomes its own character); `Conflict` for an id in use;
/// a person is needed to acknowledge.
pub(crate) fn plan(import: &Import, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
    if import.confirm.as_deref().is_none_or(str::is_empty) {
        return Err(OpError::invalid("confirm", "importing as an avatar needs the confirm token")
            .hint("run io.inspect with as: avatar on the same file and pass its confirm token"));
    }
    if import.character.is_some() {
        return Err(OpError::invalid("character", "an avatar becomes a character of its own")
            .hint("leave character out; name the new character with id"));
    }
    if let Some(id) = &import.id {
        require_free(ctx.doc, id)?;
    }
    let mut output = json!({});
    if import.acknowledge {
        ctx.require_person("only a person can acknowledge that they may perform as this model")?;
        let person = [Some(ctx.actor), ctx.confirmed_by()].into_iter().flatten().find_map(|a| match a {
            Actor::Person { name } => Some(name.clone()),
            _ => None,
        });
        output = json!({ ACKNOWLEDGED_BY: person.unwrap_or_default() });
    }
    Ok(Plan { output: Some(output), ..Plan::default() })
}

/// The character `id` must not exist yet.
fn require_free(doc: &dyn omuzaki_doc_api::DocRead, id: &CharId) -> Result<(), OpError> {
    if doc.get(&paths::character(id)).is_none() {
        return Ok(());
    }
    let mut error = OpError::conflict(format!("the project already has a character {id}"))
        .hint("pick another id, or leave id out for a free one");
    error.field = Some("id".to_owned());
    Err(error)
}

/// The work of an avatar import (see the module docs); returns the [`AvatarImported`] as JSON.
///
/// # Errors
/// As `io.inspect {as: avatar}`, plus [`CONFIRM_STALE`] and [`ACKNOWLEDGEMENT_NEEDED`] refusals, `Conflict` for an
/// object stored under other rights, and `Cancelled`.
pub(crate) fn work(import: &Import, job: &JobCtx<'_>) -> Result<Value, OpError> {
    let store = job.service::<dyn Store>()?;
    let clock: &dyn Clock = job.service::<dyn Clock>().map_or(&Unclocked, |c| c);
    let mtoon = mtoon(job.service::<Capabilities>().ok());
    let options = ImportOptions::default();
    job.checkpoint("reading", 0.0)?;
    let max_bytes = options.limits.max_input_bytes;
    let (bytes, main) = import.read_source(job, max_bytes)?;
    let (name, progress) = (import.file_name(), JobProgress(job));
    let typed = import.path.as_deref().unwrap_or_default();
    let folder = main.map(|main| FolderFiles::new(job, &main, typed, max_bytes, bytes.len() as u64));
    let resolver: &dyn omuzaki_io_api::Resolver = match &folder {
        Some(folder) => folder,
        None => &omuzaki_io_api::NoResolver,
    };
    let source =
        Source { name: &name, bytes: &bytes, format: import.format.as_ref(), resolver, progress: &progress, field: import.source_field() };
    let prepared = job.read(|doc| prepare(&source, None, &options, mtoon, clock, doc))?;
    if import.confirm.as_deref() != Some(prepared.confirm.as_str()) {
        let mut error = OpError::conflict(format!(
            "{CONFIRM_STALE}: the file, a file it refers to, or an option changed since it was inspected"
        ))
        .hint("inspect it again with io.inspect as: avatar, check the report, and import with the new token");
        error.field = Some("confirm".to_owned());
        return Err(error);
    }
    let mut def = prepared.def;
    if prepared.report.has_rule("rights.avatar_permission") {
        if !import.acknowledge {
            let mut error = OpError::conflict(format!(
                "{ACKNOWLEDGEMENT_NEEDED}: this model's terms let only its author, or people they license, perform as it"
            ))
            .hint("if that is you, import it again with acknowledge: true");
            error.field = Some("acknowledge".to_owned());
            return Err(error);
        }
        let by = job.plan_output().and_then(|o| o.get(ACKNOWLEDGED_BY)).and_then(Value::as_str).unwrap_or_default();
        def.acknowledged = Some(Acknowledgement::new(today(job.service::<dyn WallClock>().ok()), by));
    }
    let served = folder.as_ref().map(FolderFiles::served).transpose()?.unwrap_or_default();
    let (stored, took) = timed(clock, || {
        store_files(&name, &bytes, &prepared.scene, &served, &prepared.provenance, store, &progress, import.source_field())
    });
    let stored = stored.map_err(|e| if job.is_cancelled() { OpError::cancelled(job.id()) } else { e })?;
    let id = import.id.clone().unwrap_or_else(|| prepared.summary.id_suggestion.clone());
    let character_name = Some(prepared.scene.meta.name.trim())
        .filter(|n| !n.is_empty())
        .map_or_else(|| name.rsplit_once('.').map_or(name.as_str(), |(stem, _)| stem).to_owned(), str::to_owned);
    let mut row = PartSource::new(prepared.provenance.clone());
    row.terms = Some(prepared.scene.meta.terms.clone());
    let result = AvatarImported {
        asset: AssetRecord {
            original: stored.original,
            images: stored.images,
            sidecars: stored.sidecars,
            file: name.clone(),
            format: prepared.format,
            characters: std::iter::once(id.to_string()).collect(),
            provenance: Some(prepared.provenance.clone()),
        },
        id,
        name: character_name,
        def,
        row,
        summary: prepared.summary.with_timing(Stage::Store, took),
        scene: prepared.scene.summary(),
        report: prepared.report,
        provenance: prepared.provenance,
    };
    let mut value =
        serde_json::to_value(result).map_err(|e| OpError::internal(format!("serialising the import: {e}")))?;
    strip_nulls(&mut value);
    job.checkpoint("done", 1.0)?;
    Ok(value)
}

/// The patch of an avatar import: the asset record, the character with its name and avatar node, and its source row,
/// all in one patch so one undo removes them.
///
/// # Errors
/// `Internal` for a malformed result; `Conflict` when the id was taken while the job ran.
pub(crate) fn finish(ctx: &OpCtx<'_>, result: Value) -> Result<Plan, OpError> {
    let imported: AvatarImported = serde_json::from_value(result.clone())
        .map_err(|e| OpError::internal(format!("the import's result is malformed: {e}")))?;
    require_free(ctx.doc, &imported.id)?;
    let mut patch = record_patch(ctx.doc, &imported.asset)?;
    let def = serde_json::to_value(&imported.def).map_err(|e| OpError::internal(format!("writing the avatar node: {e}")))?;
    let node = json!({ "name": imported.name, "avatar": def });
    patch = patch.with(paths::character(&imported.id), None, Some(node));
    let part = PartId::new(AVATAR_PART).map_err(|e| OpError::internal(format!("the avatar part id: {e}")))?;
    let row = row_patch(ctx, &imported.id, &part, &imported.row)?;
    patch.changes.extend(row.changes);
    Ok(Plan { patch, output: Some(result), ..Plan::default() })
}

/// Today's date in UTC, `YYYY-MM-DD`, from the host's wall clock (the system's when the host offers none).
fn today(clock: Option<&dyn WallClock>) -> String {
    let millis = clock.map_or_else(system_millis, |c| c.now().0);
    let days = i64::try_from(millis / 86_400_000).unwrap_or(0);
    let (y, m, d) = civil_from_days(days);
    format!("{y:04}-{m:02}-{d:02}")
}

/// Milliseconds since the Unix epoch on the system clock (0 if it reads before the epoch).
fn system_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

/// The proleptic Gregorian date of `days` since 1970-01-01 (Howard Hinnant's `civil_from_days`).
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, u32::try_from(month).unwrap_or(1), u32::try_from(day).unwrap_or(1))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn days_become_calendar_dates() {
        assert_eq!(civil_from_days(0), (1970, 1, 1));
        assert_eq!(civil_from_days(19_675), (2023, 11, 14));
        assert_eq!(civil_from_days(11_016), (2000, 2, 29), "a leap day");
        assert_eq!(civil_from_days(20_741), (2026, 10, 15));
    }
}
