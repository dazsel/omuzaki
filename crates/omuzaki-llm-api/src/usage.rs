//! Usage, money and the offline cost bound (llm-api D4). Money is integer micro-dollars, rounded up once per turn.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::message::{ContentBlock, ImageMime, TurnRequest};

/// Tokens a turn used. `input` excludes the two cache counters.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Usage {
    /// Prompt tokens that were neither read from nor written to the cache.
    pub input: u64,
    /// Tokens the model produced.
    pub output: u64,
    /// Prompt tokens read from the cache.
    pub cache_read: u64,
    /// Prompt tokens written to the 5-minute cache.
    pub cache_write: u64,
}

impl Usage {
    /// The whole prompt: `input + cache_read + cache_write`.
    pub fn prompt(&self) -> u64 {
        self.input.saturating_add(self.cache_read).saturating_add(self.cache_write)
    }

    /// Every counter: `prompt() + output`. This is what a token budget counts.
    pub fn tokens(&self) -> u64 {
        self.prompt().saturating_add(self.output)
    }
}

/// Millionths of a US dollar.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
pub struct MicroUsd(pub u64);

/// µ$ per million tokens, for each counter.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Rates {
    /// Plain prompt tokens.
    pub input: u64,
    /// Output tokens.
    pub output: u64,
    /// Cache reads.
    pub cache_read: u64,
    /// 5-minute cache writes.
    pub cache_write: u64,
}

/// The rates that apply up to a prompt size.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PriceTier {
    /// The largest prompt (in tokens) this tier holds; `None` holds any.
    pub prompt_up_to: Option<u64>,
    /// The rates.
    pub rates: Rates,
}

/// A dated price list for one model.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PriceTable {
    /// `YYYY-MM-DD` the prices were read, or `local` for a zero table.
    pub as_of: String,
    /// Where they were read.
    pub source: String,
    /// Tiers, smallest first; the first one that holds the prompt applies.
    pub tiers: Vec<PriceTier>,
}

impl PriceTable {
    /// The rates for a prompt of `prompt` tokens: the first tier that holds it, else the last tier (a table that
    /// ends in a bounded tier is dearest there), else zero for a table with no tiers.
    pub fn rates_for(&self, prompt: u64) -> Rates {
        let hit = self.tiers.iter().find(|t| t.prompt_up_to.is_none_or(|max| prompt <= max));
        hit.or(self.tiers.last()).map(|t| t.rates).unwrap_or_default()
    }
}

/// µ$ of `sum` token-times-rate units (µ$ per million tokens), rounded up.
fn round_up(sum: u128) -> MicroUsd {
    MicroUsd(u64::try_from(sum.div_ceil(1_000_000)).unwrap_or(u64::MAX))
}

/// How a model counts an image.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub enum ImageCost {
    /// ⌈width / px⌉ × ⌈height / px⌉ tokens (Anthropic: 28).
    Tiles {
        /// The tile side in pixels.
        px: u32,
    },
    /// The same for every image.
    Flat(u64),
}

impl ImageCost {
    fn tokens(self, (w, h): (u32, u32)) -> u64 {
        match self {
            Self::Tiles { px } => u64::from(w.div_ceil(px.max(1))) * u64::from(h.div_ceil(px.max(1))),
            Self::Flat(n) => n,
        }
    }
}

/// What one model can do and how to bound its cost.
// Four independent capabilities, each a plain yes or no that a provider reports; an enum set would only rename them.
#[allow(clippy::struct_excessive_bools)]
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ModelInfo {
    /// The model id the provider serves.
    pub id: String,
    /// The context window in tokens.
    pub context_window: u64,
    /// The most output tokens one turn may produce.
    pub max_output: u64,
    /// Accepts images.
    pub vision: bool,
    /// Accepts tools.
    pub tools: bool,
    /// Honours [`Effort`](crate::message::Effort).
    pub effort: bool,
    /// Honours cache breakpoints.
    pub caching: bool,
    /// How an image counts toward the prompt.
    pub image_cost: ImageCost,
}

/// The largest image the contract allows on its long side, used when a format's size cannot be read.
const MAX_IMAGE_SIDE: u32 = 768;
/// Tokens added per block for framing.
const BLOCK_OVERHEAD: u64 = 8;

/// The pixel size of an image, from its header; the contract's maximum when the format is not read here.
fn image_size(mime: ImageMime, bytes: &[u8]) -> (u32, u32) {
    let be = |at: usize| bytes.get(at..at + 4).and_then(|b| <[u8; 4]>::try_from(b).ok()).map(u32::from_be_bytes);
    let le = |at: usize| bytes.get(at..at + 2).and_then(|b| <[u8; 2]>::try_from(b).ok()).map(u16::from_le_bytes);
    let read = match mime {
        ImageMime::Png => be(16).zip(be(20)),
        ImageMime::Gif => le(6).zip(le(8)).map(|(w, h)| (u32::from(w), u32::from(h))),
        _ => None,
    };
    read.unwrap_or((MAX_IMAGE_SIDE, MAX_IMAGE_SIDE))
}

fn block_tokens(block: &ContentBlock, info: &ModelInfo) -> u64 {
    let body = match block {
        ContentBlock::Text(t) => t.len() as u64,
        ContentBlock::Image { mime, bytes, .. } => info.image_cost.tokens(image_size(*mime, bytes)),
        ContentBlock::ToolUse { id, name, input } => (id.len() + name.len() + input.to_string().len()) as u64,
        ContentBlock::ToolResult { blocks, .. } => blocks.iter().map(|b| block_tokens(b, info)).sum(),
        ContentBlock::Opaque { block, .. } => block.to_string().len() as u64,
    };
    body + BLOCK_OVERHEAD
}

/// What the ledger measured on the last turn: the tokens of that prompt plus its answer, and how many messages of
/// the next request they cover (the request's own, plus the answer).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
struct Measured {
    tokens: u64,
    messages: usize,
    /// A fingerprint of the system, the tools and the request's own messages that the measurement covers.
    prefix: u64,
}

/// FNV-1a over the JSON of the parts of `request` that the measured tokens cover: the system, the tools and the
/// first `messages` messages. Fixed constants, so a stored ledger compares the same across runs.
fn fingerprint(request: &TurnRequest, messages: usize) -> u64 {
    let covered = request.messages.get(..messages).unwrap_or(&request.messages);
    let json = serde_json::to_vec(&(&request.system, &request.tools, covered)).unwrap_or_default();
    json.iter().fold(0xcbf2_9ce4_8422_2325, |hash, b| (hash ^ u64::from(*b)).wrapping_mul(0x0100_0000_01b3))
}

/// Bounds a turn's cost before it is sent, with no network (llm-api D4). One ledger per conversation.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Ledger {
    measured: Option<Measured>,
}

impl Ledger {
    /// An empty ledger: the first bound estimates everything.
    pub fn new() -> Self {
        Self::default()
    }

    /// Records a finished turn: its request and final usage. The next bound starts from the measured prompt and
    /// the answer, and estimates only what is appended after them.
    pub fn observe(&mut self, request: &TurnRequest, usage: &Usage) {
        self.measured = Some(Measured {
            tokens: usage.prompt().saturating_add(usage.output),
            messages: request.messages.len() + 1,
            prefix: fingerprint(request, request.messages.len()),
        });
    }

    /// An upper estimate of the prompt of `request`, in tokens: the measured history plus an estimate of what was
    /// appended (1 token per byte of text and JSON, 8 per block, images from [`ModelInfo::image_cost`]), or an
    /// estimate of everything on a first turn, when the history is shorter than the ledger remembers, or when the
    /// system, the tools or an earlier message changed since the measurement.
    pub fn prompt_estimate(&self, request: &TurnRequest, info: &ModelInfo) -> u64 {
        let messages = |from: usize| -> u64 {
            let appended = request.messages.iter().skip(from);
            appended.flat_map(|m| &m.content).map(|b| block_tokens(b, info)).sum()
        };
        let same_prefix = |m: &Measured| fingerprint(request, m.messages - 1) == m.prefix;
        if let Some(m) = self.measured.filter(|m| m.messages <= request.messages.len() && same_prefix(m)) {
            return m.tokens.saturating_add(messages(m.messages));
        }
        let system: u64 = request.system.iter().map(|s| s.text.len() as u64 + BLOCK_OVERHEAD).sum();
        let tools: u64 = request
            .tools
            .iter()
            .map(|t| (t.name.len() + t.description.len() + t.input_schema.to_string().len()) as u64 + BLOCK_OVERHEAD)
            .sum();
        system.saturating_add(tools).saturating_add(messages(0))
    }

    /// The most `request` can cost: the whole prompt at the dearest of `input`, `cache_read` and `cache_write` in the
    /// tier it falls in (any of them could bill a prompt token, so the bound never under-estimates), plus `max_output` at the output rate, rounded up to a micro-dollar. The host sends only if this fits.
    pub fn bound(&self, request: &TurnRequest, info: &ModelInfo, prices: &PriceTable) -> MicroUsd {
        let prompt = self.prompt_estimate(request, info);
        let rates = prices.rates_for(prompt);
        let per_token = u128::from(rates.input.max(rates.cache_read).max(rates.cache_write));
        round_up(u128::from(prompt) * per_token + u128::from(request.max_output) * u128::from(rates.output))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::{CachePlan, Effort, Message, Role, SystemBlock, ToolChoice};

    fn flat(rate: u64) -> PriceTable {
        let rates = Rates { input: rate, output: 0, cache_read: 0, cache_write: 0 };
        PriceTable {
            as_of: "local".into(),
            source: String::new(),
            tiers: vec![PriceTier { prompt_up_to: None, rates }],
        }
    }

    fn request(system: &str, texts: &[&str]) -> TurnRequest {
        let message = |t: &&str| Message { role: Role::User, content: vec![ContentBlock::Text((*t).into())] };
        TurnRequest {
            system: vec![SystemBlock { text: system.into(), cache_after: false }],
            messages: texts.iter().map(message).collect(),
            tools: Vec::new(),
            tool_choice: ToolChoice::Auto,
            max_output: 10,
            effort: Effort::Default,
            cache: CachePlan { conversation: false },
        }
    }

    fn model() -> ModelInfo {
        ModelInfo {
            id: "m".into(),
            context_window: 1000,
            max_output: 10,
            vision: false,
            tools: false,
            effort: false,
            caching: false,
            image_cost: ImageCost::Flat(1),
        }
    }

    #[test]
    fn measured_tokens_hold_only_while_the_prefix_is_unchanged() {
        let first = request("sys", &["a"]);
        let mut ledger = Ledger::new();
        ledger.observe(&first, &Usage { input: 5_000, output: 100, cache_read: 0, cache_write: 0 });
        let mut next = request("sys", &["a", "answer", "b"]);
        assert_eq!(ledger.prompt_estimate(&next, &model()), 5_100 + 1 + BLOCK_OVERHEAD);
        // A changed system prompt, or an edited earlier message, makes the measurement stale: estimate it all.
        let edited = request("other", &["a", "answer", "b"]);
        assert!(ledger.prompt_estimate(&edited, &model()) < 100);
        next.messages[0] = request("sys", &["changed"]).messages.remove(0);
        assert!(ledger.prompt_estimate(&next, &model()) < 100);
    }

    #[test]
    fn counters_add_without_overflow() {
        let u = Usage { input: u64::MAX, output: 1, cache_read: 1, cache_write: 1 };
        assert_eq!((u.prompt(), u.tokens()), (u64::MAX, u64::MAX));
    }

    #[test]
    fn the_first_tier_that_holds_the_prompt_applies_and_the_last_catches_the_rest() {
        let cheap = Rates { input: 1, ..Rates::default() };
        let dear = Rates { input: 2, ..Rates::default() };
        let table = PriceTable {
            as_of: "x".into(),
            source: String::new(),
            tiers: vec![
                PriceTier { prompt_up_to: Some(100), rates: cheap },
                PriceTier { prompt_up_to: Some(200), rates: dear },
            ],
        };
        assert_eq!(table.rates_for(100), cheap);
        assert_eq!(table.rates_for(101), dear);
        assert_eq!(table.rates_for(9_999), dear);
        assert_eq!(PriceTable { tiers: Vec::new(), ..table }.rates_for(1), Rates::default());
    }

    #[test]
    fn the_bound_prices_the_prompt_at_the_dearest_rate_even_when_cache_reads_cost_more() {
        let rates = Rates { input: 1_000_000, output: 0, cache_read: 5_000_000, cache_write: 2_000_000 };
        let prices = PriceTable {
            as_of: "x".into(),
            source: String::new(),
            tiers: vec![PriceTier { prompt_up_to: None, rates }],
        };
        let info = ModelInfo {
            id: "m".into(),
            context_window: 1000,
            max_output: 0,
            vision: false,
            tools: false,
            effort: false,
            caching: true,
            image_cost: ImageCost::Flat(1),
        };
        let request = TurnRequest {
            system: Vec::new(),
            messages: vec![crate::Message {
                role: crate::Role::User,
                content: vec![ContentBlock::Text("abcd".into())],
            }],
            tools: Vec::new(),
            tool_choice: crate::ToolChoice::Auto,
            max_output: 0,
            effort: crate::Effort::Default,
            cache: crate::CachePlan { conversation: false },
        };
        let prompt = Ledger::new().prompt_estimate(&request, &info);
        assert!(prompt > 0);
        assert_eq!(Ledger::new().bound(&request, &info, &prices), MicroUsd(prompt * 5));
    }

    #[test]
    fn money_rounds_up_once() {
        assert_eq!(round_up(1), MicroUsd(1));
        assert_eq!(round_up(1_000_000), MicroUsd(1));
        assert_eq!(round_up(1_000_001), MicroUsd(2));
        assert_eq!(round_up(0), MicroUsd(0));
        assert_eq!(flat(0).rates_for(5).input, 0);
    }

    #[test]
    fn an_unreadable_image_is_bounded_at_the_contract_maximum() {
        assert_eq!(image_size(ImageMime::Jpeg, &[]), (768, 768));
        assert_eq!(ImageCost::Tiles { px: 28 }.tokens((768, 768)), 784);
        assert_eq!(ImageCost::Flat(5).tokens((1, 1)), 5);
    }
}
