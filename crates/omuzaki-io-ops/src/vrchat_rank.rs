//! `VRChat`'s avatar performance rank, worked out from an avatar's counts (VF-07).
//!
//! A thin wrapper over the limit-table engine in [`crate::limits`]: the limits are `VRChat`'s published Avatar
//! Performance Ranking numbers, which live only in `data/platforms/vrchat-pc.toml` and `vrchat-quest.toml` with the
//! source URL and the date they were read. [`rank`] evaluates an avatar against the table for its [`Platform`] and
//! reads the tier as a [`Rank`]: the tiers Excellent, Good, Medium and Poor, and Very Poor past the last. A stat the
//! page lists as disabled on a platform is [`StatRank::rank`] `None` (Unsupported) and does not count toward the rank.
//! The three bounds axes are one stat here, `bounds`, ranked by its worst axis.
//!
//! [`stats_from_ioscene`] counts what an export carries; some stats are things an export never writes, so they are 0.
//! Texture memory is an estimate, see [`texture_mib`].

pub use crate::limits::{AvatarStats, LimitsError, StatsError, stats_from_ioscene};
use crate::limits::{StatTier, builtin_checked, evaluate, stat_def};

crate::register_limit_table!("vrchat-pc", "../data/platforms/vrchat-pc.toml");
crate::register_limit_table!("vrchat-quest", "../data/platforms/vrchat-quest.toml");

/// The tiers of the `VRChat` tables, best first, as ranks. Very Poor is past the last tier.
const TIER_RANKS: [Rank; 4] = [Rank::Excellent, Rank::Good, Rank::Medium, Rank::Poor];
/// The registry keys of the bounds axes, which this module ranks as one stat.
const BOUNDS_AXES: [&str; 3] = ["bounds_x", "bounds_y", "bounds_z"];
const BOUNDS: &str = "bounds";

/// The platform whose limits apply.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum Platform {
    /// Windows and other desktop `VRChat`.
    Pc,
    /// Android (the page's mobile table; iOS shares it).
    Android,
}

/// An avatar's rank. Ordered from best to worst, so the overall rank is the maximum of the stat ranks.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Rank {
    /// At or under the Excellent limit.
    Excellent,
    /// Over Excellent, at or under Good.
    Good,
    /// Over Good, at or under Medium.
    Medium,
    /// Over Medium, at or under Poor.
    Poor,
    /// Over Poor. The page leaves this one unbounded.
    VeryPoor,
}

impl Rank {
    /// The page's name for the rank.
    pub const fn name(self) -> &'static str {
        match self {
            Self::Excellent => "Excellent",
            Self::Good => "Good",
            Self::Medium => "Medium",
            Self::Poor => "Poor",
            Self::VeryPoor => "Very Poor",
        }
    }
}

/// The rank of one stat. `rank` is `None` when the platform does not support the stat.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub struct StatRank {
    /// The stat's key in the data file.
    pub name: &'static str,
    /// The stat's rank, or `None` (Unsupported): the page says the component is disabled on this platform, so the
    /// platform strips it and it does not count toward the avatar's rank.
    pub rank: Option<Rank>,
}

/// An avatar's overall rank and the rank of every stat.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ranking {
    /// The worst rank among the supported stats.
    pub rank: Rank,
    /// Every stat in the data file, in its order.
    pub stats: Vec<StatRank>,
}

impl Platform {
    /// The id of the limit table for this platform.
    const fn table_id(self) -> &'static str {
        match self {
            Self::Pc => "vrchat-pc",
            Self::Android => "vrchat-quest",
        }
    }
}

/// The rank for a tier index, or `None` for a tier the `VRChat` page does not have.
fn rank_of_tier(tier: Option<usize>) -> Result<Rank, LimitsError> {
    match tier {
        None => Ok(Rank::VeryPoor),
        Some(i) => TIER_RANKS.get(i).copied().ok_or_else(|| {
            LimitsError::new("data/platforms", None, format!("has a tier {i} the VRChat page does not"))
        }),
    }
}

/// Ranks an avatar on `platform`: each supported stat against its limits, the overall rank the worst of them.
///
/// # Errors
///
/// Fails only if the bundled limits file is broken, which the tests catch.
pub fn rank(stats: &AvatarStats, platform: Platform) -> Result<Ranking, LimitsError> {
    let table = builtin_checked(platform.table_id())?;
    let eval = evaluate(stats, table);
    let mut per_stat: Vec<StatRank> = Vec::with_capacity(eval.stats.len());
    for verdict in &eval.stats {
        let Some(def) = stat_def(&verdict.key) else {
            return Err(LimitsError::new(platform.table_id(), Some(&verdict.key), "is not a stat"));
        };
        let rank = match verdict.tier {
            StatTier::Within(i) => Some(rank_of_tier(Some(i))?),
            StatTier::Beyond => Some(Rank::VeryPoor),
            StatTier::Unsupported => None,
        };
        let name = if BOUNDS_AXES.contains(&def.key) { BOUNDS } else { def.key };
        match per_stat.iter_mut().find(|s| s.name == name) {
            Some(merged) => merged.rank = merged.rank.max(rank),
            None => per_stat.push(StatRank { name, rank }),
        }
    }
    Ok(Ranking { rank: rank_of_tier(eval.tier)?, stats: per_stat })
}

/// The texture memory the platform would upload for the avatar's images, in MiB.
///
/// An estimate of what Unity's default import uploads: PC uses 0.5 bytes per pixel for opaque images and 1 with alpha
/// (DXT1 and DXT5 or BC7), Android uses ASTC 6x6 at 3.56 bits per pixel, and a full mip chain adds a third. The rates
/// are the `[texture]` block of the platform's table. A real import can differ, so treat the rank of a texture-heavy
/// avatar as approximate. If the platform's table were broken the estimate is infinite, which ranks Very Poor rather
/// than passing; the tests catch a broken table first.
pub fn texture_mib(stats: &AvatarStats, platform: Platform) -> f64 {
    match builtin_checked(platform.table_id()) {
        Ok(table) => stats.texture_mib(&table.texture),
        Err(_) => f64::INFINITY,
    }
}
