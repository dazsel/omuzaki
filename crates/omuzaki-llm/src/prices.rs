//! The shipped model table (`prices.toml`, llm-api D4, D6): what each Anthropic model can do and what it costs, as
//! data a reviewed change updates.

use std::sync::OnceLock;

use omuzaki_llm_api::error::LlmError;
use omuzaki_llm_api::usage::{ImageCost, ModelInfo, PriceTable, PriceTier, Rates};
use serde::Deserialize;

/// Anthropic counts an image in tiles of 28 pixels (retrieved 2026-10-13 from docs.claude.com/en/docs/build-with-claude/vision).
pub(crate) const ANTHROPIC_IMAGE_TILE: u32 = 28;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    model: Vec<Row>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Row {
    id: String,
    as_of: String,
    source: String,
    context_window: u64,
    max_output: u64,
    tiers: Vec<TierRow>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TierRow {
    prompt_up_to: Option<u64>,
    input: u64,
    output: u64,
    cache_read: u64,
    cache_write: u64,
}

/// One priced model.
#[derive(Clone, Debug, PartialEq)]
pub struct ModelEntry {
    /// What the model can do.
    pub info: ModelInfo,
    /// What it costs.
    pub prices: PriceTable,
}

impl From<Row> for ModelEntry {
    fn from(row: Row) -> Self {
        // Every model in the table takes images and tools, honours effort and caches prefixes (see the file's header).
        let info = ModelInfo::new(row.id, row.context_window, row.max_output, ImageCost::tiles(ANTHROPIC_IMAGE_TILE))
            .with_vision(true)
            .with_tools(true)
            .with_effort(true)
            .with_caching(true);
        let tier = |t: TierRow| {
            PriceTier::new(
                t.prompt_up_to,
                Rates::new(t.input, t.output).with_cache_read(t.cache_read).with_cache_write(t.cache_write),
            )
        };
        let prices = PriceTable::new(row.as_of, row.source, row.tiers.into_iter().map(tier).collect());
        Self { info, prices }
    }
}

static TABLE: OnceLock<Result<Vec<ModelEntry>, String>> = OnceLock::new();

/// Every priced Anthropic model, in the order of the file.
///
/// # Errors
/// `Malformed` if `prices.toml` does not parse (a test keeps that from shipping).
pub fn anthropic_models() -> Result<&'static [ModelEntry], LlmError> {
    let table = TABLE.get_or_init(|| {
        let file: File = toml::from_str(include_str!("prices.toml")).map_err(|e| e.to_string())?;
        Ok(file.model.into_iter().map(ModelEntry::from).collect())
    });
    table.as_deref().map_err(|why| LlmError::Malformed(format!("prices.toml: {why}")))
}

/// The priced Anthropic model `id`, if the table has it.
///
/// # Errors
/// As [`anthropic_models`].
pub fn anthropic_model(id: &str) -> Result<Option<&'static ModelEntry>, LlmError> {
    Ok(anthropic_models()?.iter().find(|m| m.info.id == id))
}

/// The prices of a model that runs on this machine: nothing, dated `local`.
pub fn local_prices() -> PriceTable {
    PriceTable::new("local", "runs on this machine", vec![PriceTier::new(None, Rates::default())])
}

#[cfg(test)]
mod tests {
    use super::*;
    use omuzaki_llm_api::usage::{MicroUsd, Usage};

    #[test]
    fn the_shipped_table_parses_and_lists_the_four_current_models() {
        let ids: Vec<&str> =
            anthropic_models().unwrap_or_else(|e| panic!("{e}")).iter().map(|m| m.info.id.as_str()).collect();
        assert_eq!(ids, ["claude-fable-5-1", "claude-opus-5-5", "claude-sonnet-5-5", "claude-haiku-5-5"]);
    }

    #[test]
    fn every_table_is_dated_sourced_and_priced_for_every_prompt() {
        for m in anthropic_models().unwrap_or_else(|e| panic!("{e}")) {
            assert_eq!(m.prices.as_of.len(), 10, "{} is not dated YYYY-MM-DD", m.info.id);
            assert!(m.prices.source.starts_with("https://"), "{}", m.info.id);
            let last = m.prices.tiers.last().unwrap_or_else(|| panic!("{} has no tier", m.info.id));
            assert_eq!(last.prompt_up_to, None, "{}: the last tier must catch every prompt", m.info.id);
            assert!(m.info.vision && m.info.tools && m.info.effort && m.info.caching);
        }
    }

    #[test]
    fn haiku_is_priced_by_prompt_length_as_in_the_worked_example() {
        let haiku =
            anthropic_model("claude-haiku-5-5").unwrap_or_else(|e| panic!("{e}")).unwrap_or_else(|| panic!("missing"));
        let turn_one = Usage::new(37, 58).with_cache_write(4800);
        let turn_two = Usage::new(413, 23).with_cache_read(4800);
        assert_eq!(turn_one.cost(&haiku.prices), MicroUsd(633));
        assert_eq!(turn_two.cost(&haiku.prices), MicroUsd(101));
        // Over 100,000 prompt tokens the dearer tier applies: 100,001 input tokens at 0.50 $/MTok.
        assert_eq!(Usage::new(100_001, 0).cost(&haiku.prices), MicroUsd(50_001));
    }

    #[test]
    fn an_unlisted_model_is_not_in_the_table() {
        assert_eq!(anthropic_model("claude-unknown-9"), Ok(None));
        assert_eq!(local_prices().as_of, "local");
    }
}
