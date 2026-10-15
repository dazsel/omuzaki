//! Money and privacy before anything runs (CN-04; D4, D5): the [`Estimate`] a connector gives offline, and the
//! [`Preview`] a person or agent sees, with the rule ([`Preview::widens`]) that refuses a run that would send more,
//! to more places, or cost more than the plan showed.

use omuzaki_store_api::Commercial;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::SecretLabel;
use crate::id::{Capability, ConnectorId};
use crate::manifest::{ConnectorManifest, CostModel, Sends, Tri, Unit};
use crate::output::{ConnectError, usd};
use crate::request::{MAX_SECONDS, Request};

/// The input tokens the token bound counts for each image (D2): a conservative placeholder above what providers bill
/// for a large image.
pub const TOKENS_PER_IMAGE: u64 = 1_600;
/// Text bytes per input token in the bound: fewer than real tokenizers average, so the bound stays above the bill.
pub const BYTES_PER_TOKEN: u64 = 3;

/// Money and time before anything runs. Integer micro-dollars, so sums are exact (agent-sessions 8.1).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Estimate {
    /// Micro-dollars: the price, or an upper bound when `exact` is false.
    pub usd_micros: u64,
    /// True when `usd_micros` is the price, false when it is an upper bound.
    pub exact: bool,
    /// How it was worked out, in short English: "4 images x $0.04".
    pub basis: String,
    /// Expected wall time, when the connector knows.
    pub seconds: Option<f32>,
}

impl Estimate {
    /// Nothing to pay.
    pub fn free() -> Self {
        Self { usd_micros: 0, exact: true, basis: "free".to_owned(), seconds: None }
    }
}

/// The default every connector can use: from the manifest's cost model and the request.
///
/// `Free` is 0; `PerCall` the price; `PerUnit` the price times the quantity (images and meshes: `count`; a call: 1;
/// seconds: a music request's `seconds` times `count`, a motion request's `seconds` or [`MAX_SECONDS`]; thousand
/// characters: the prompt's characters over 1,000, rounded up). `PerToken` is an upper bound: input tokens are the
/// text bytes over [`BYTES_PER_TOKEN`], rounded up, plus [`TOKENS_PER_IMAGE`] per image; output tokens are
/// `max_tokens`. Arithmetic saturates rather than wraps.
///
/// # Errors
/// [`ConnectError::Unsupported`] for `CostModel::Provider` (the connector prices from its provider) and for a
/// per-second price on a request that has no length.
pub fn estimate_from_manifest(m: &ConnectorManifest, req: &Request) -> Result<Estimate, ConnectError> {
    match &m.cost {
        CostModel::Free => Ok(Estimate::free()),
        CostModel::PerCall { usd_micros, .. } => Ok(Estimate {
            usd_micros: *usd_micros,
            exact: true,
            basis: format!("1 call x {}", usd(*usd_micros)),
            seconds: None,
        }),
        CostModel::PerUnit { unit, usd_micros, .. } => {
            let (quantity, exact) = units(*unit, req)?;
            Ok(Estimate {
                usd_micros: quantity.saturating_mul(*usd_micros),
                exact,
                basis: format!("{} x {}", unit_words(*unit, quantity), usd(*usd_micros)),
                seconds: None,
            })
        }
        CostModel::PerToken { input_usd_micros_per_mtok, output_usd_micros_per_mtok, .. } => {
            let images = req.input_assets().len() as u64;
            let input =
                req.text_bytes().div_ceil(BYTES_PER_TOKEN).saturating_add(images.saturating_mul(TOKENS_PER_IMAGE));
            let output = u64::from(req.max_tokens().unwrap_or(0));
            let side = |tokens: u64, price: u64| {
                let micros = (u128::from(tokens) * u128::from(price)).div_ceil(1_000_000);
                u64::try_from(micros).unwrap_or(u64::MAX)
            };
            Ok(Estimate {
                usd_micros: side(input, *input_usd_micros_per_mtok)
                    .saturating_add(side(output, *output_usd_micros_per_mtok)),
                exact: false,
                basis: format!("at most {input} input and {output} output tokens"),
                seconds: None,
            })
        }
        CostModel::Provider => {
            Err(ConnectError::unsupported("this connector's prices are its provider's; ask the connector to estimate"))
        }
    }
}

/// The billable quantity of `req` in `unit`, and whether it is exact.
fn units(unit: Unit, req: &Request) -> Result<(u64, bool), ConnectError> {
    Ok(match (unit, req) {
        (Unit::Call, _) => (1, true),
        (Unit::Image | Unit::Mesh, _) => (u64::from(req.count()), true),
        (Unit::Second, Request::Music(r)) => (u64::from(r.seconds) * u64::from(r.count), true),
        (Unit::Second, Request::Motion(r)) => (u64::from(r.seconds.unwrap_or(MAX_SECONDS)), r.seconds.is_some()),
        (Unit::Second, _) => {
            return Err(ConnectError::unsupported(format!(
                "a per-second price needs a music or motion request, not {}",
                req.capability()
            )));
        }
        (Unit::ThousandChars, _) => {
            let chars = req.prompt_text().map_or(0, |t| t.chars().count() as u64);
            (chars.div_ceil(1_000), true)
        }
    })
}

/// "4 images", "1 call", "3 thousand characters".
fn unit_words(unit: Unit, n: u64) -> String {
    let (one, many) = match unit {
        Unit::Call => ("call", "calls"),
        Unit::Image => ("image", "images"),
        Unit::Second => ("second", "seconds"),
        Unit::ThousandChars => ("thousand characters", "thousand characters"),
        Unit::Mesh => ("mesh", "meshes"),
    };
    format!("{n} {}", if n == 1 { one } else { many })
}

/// What one request sends out, by kind.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Outbound {
    /// What it is.
    pub what: Sends,
    /// How many.
    pub count: u32,
    /// How many bytes in all.
    pub bytes: u64,
}

/// Whether a connector can run now.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "status", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Status {
    /// It can run.
    Ready,
    /// A person has not enabled it (every connector starts disabled, D12).
    Disabled,
    /// Its key is not in the keychain.
    NeedsKey,
    /// The configured plan tier cannot produce shippable output.
    #[non_exhaustive]
    NeedsPlan {
        /// The plan tiers that can.
        allowed: Vec<String>,
    },
    /// Its settings are incomplete (an LLM connector with no provider selected).
    Unconfigured,
}

impl Status {
    /// [`Status::NeedsPlan`].
    pub fn needs_plan(allowed: Vec<String>) -> Self {
        Self::NeedsPlan { allowed }
    }
}

/// Everything the person sees before a call runs (CN-04). Data, not prose: the UI words it in the person's language;
/// [`english`](Self::english) words it for agents and logs.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Preview {
    /// The chosen connector.
    pub connector: ConnectorId,
    /// Its name.
    pub name: String,
    /// The capability asked for.
    pub capability: Capability,
    /// Whether anything leaves this computer, decided on the effective endpoint (D12).
    pub leaves_machine: bool,
    /// Where it goes: hosts, or `stdio`.
    pub destinations: Vec<String>,
    /// The effective endpoint, for an HTTP connector.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
    /// What is sent, by kind.
    pub outbound: Vec<Outbound>,
    /// The money.
    pub estimate: Estimate,
    /// The vendor's words on what it keeps.
    pub retention: String,
    /// Whether the vendor may train on it.
    pub trains_on_inputs: Tri,
    /// What the outputs would be on record (the join the host computed).
    pub commercial: Commercial,
    /// The licence the outputs would carry.
    pub licence: String,
    /// The plan tier on record.
    pub plan: Option<String>,
    /// The key label this call uses, never a value ([`Target::secret`](crate::Target::secret)).
    pub secret: Option<SecretLabel>,
    /// Whether the plan costs money, so an agent needs the `spend` scope (D5).
    pub needs_spend: bool,
    /// Set when an agent would send project assets off the computer: the sentence a person must confirm.
    pub needs_person: Option<String>,
    /// Things to know: an unencrypted LAN endpoint, a plan that makes outputs non-commercial.
    pub warnings: Vec<String>,
}

/// How a run would go further than the preview its plan showed (D4). Any of these fails the run with
/// [`RunError::PreviewChanged`](crate::RunError::PreviewChanged) before a request is sent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "widening", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Widening {
    /// It would leave the computer; the plan said it stays.
    LeavesMachine,
    /// It would reach a destination the plan did not list.
    #[non_exhaustive]
    Destination {
        /// The new destination.
        destination: String,
    },
    /// It would use another endpoint.
    #[non_exhaustive]
    Endpoint {
        /// The new endpoint.
        endpoint: String,
    },
    /// It would send more of one kind, by count or bytes.
    #[non_exhaustive]
    Outbound {
        /// The kind.
        what: Sends,
    },
    /// The vendor's training answer changed.
    TrainsOnInputs,
    /// The vendor's retention words changed.
    Retention,
    /// It would use another key label.
    Secret,
    /// It now needs a person's confirmation that the plan did not ask for.
    NeedsPerson,
    /// The plan tier changed.
    Plan,
    /// It would cost more.
    #[non_exhaustive]
    Estimate {
        /// What the plan showed, micro-dollars.
        planned_usd_micros: u64,
        /// What it would cost now, micro-dollars.
        now_usd_micros: u64,
    },
}

impl Widening {
    /// [`Widening::Destination`].
    pub fn destination(destination: impl Into<String>) -> Self {
        Self::Destination { destination: destination.into() }
    }

    /// [`Widening::Endpoint`].
    pub fn endpoint(endpoint: impl Into<String>) -> Self {
        Self::Endpoint { endpoint: endpoint.into() }
    }

    /// [`Widening::Outbound`].
    pub fn outbound(what: Sends) -> Self {
        Self::Outbound { what }
    }

    /// [`Widening::Estimate`].
    pub fn estimate(planned_usd_micros: u64, now_usd_micros: u64) -> Self {
        Self::Estimate { planned_usd_micros, now_usd_micros }
    }
}

impl std::fmt::Display for Widening {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Widening::LeavesMachine => f.write_str("it would now leave this computer"),
            Widening::Destination { destination } => write!(f, "it would now also go to {destination}"),
            Widening::Endpoint { endpoint } => write!(f, "its endpoint is now {endpoint}"),
            Widening::Outbound { what } => write!(f, "it would now send more {}", sends_noun(*what, 2)),
            Widening::TrainsOnInputs => f.write_str("the vendor's training terms changed"),
            Widening::Retention => f.write_str("the vendor's retention terms changed"),
            Widening::Secret => f.write_str("it would now use another key"),
            Widening::NeedsPerson => f.write_str("it now needs a person to confirm what leaves this computer"),
            Widening::Plan => f.write_str("the plan tier changed"),
            Widening::Estimate { planned_usd_micros, now_usd_micros } => {
                write!(f, "it would now cost {} instead of {}", usd(*now_usd_micros), usd(*planned_usd_micros))
            }
        }
    }
}

impl Preview {
    /// The preview in one or more English sentences, for agents and logs:
    ///
    /// - stays: "This stays on this computer and costs nothing."
    /// - leaves: "This sends 3 images and a prompt to api.example.com and costs $0.12." followed by the vendor's
    ///   retention words and, unless the answer is no, what it says about training.
    ///
    /// An upper-bound estimate reads "costs at most".
    pub fn english(&self) -> String {
        let cost = match (self.estimate.usd_micros, self.estimate.exact) {
            (0, true) => "costs nothing".to_owned(),
            (m, true) => format!("costs {}", usd(m)),
            (m, false) => format!("costs at most {}", usd(m)),
        };
        if !self.leaves_machine {
            return format!("This stays on this computer and {cost}.");
        }
        let what: Vec<String> = self.outbound.iter().map(|o| counted(o.what, o.count)).collect();
        let what = if what.is_empty() { "a request".to_owned() } else { join_and(&what) };
        let mut s = format!("This sends {what} to {} and {cost}.", join_and(&self.destinations));
        let retention = self.retention.trim();
        if !retention.is_empty() {
            s.push(' ');
            s.push_str(retention);
            if !retention.ends_with('.') {
                s.push('.');
            }
        }
        match self.trains_on_inputs {
            Tri::No => {}
            Tri::Yes => s.push_str(" It may train on what it is sent."),
            Tri::Unknown => s.push_str(" Its terms don't say whether it trains on what it is sent."),
        }
        s
    }

    /// How `self`, built now, goes further than `planned`, the preview the plan showed; `None` when it does not.
    /// Narrower changes (stricter rights, fewer bytes, a lower price) pass. Checked in this order: leaving the
    /// computer, destinations, endpoint, what is sent, training, retention, key, confirmation, plan tier, money.
    pub fn widens(&self, planned: &Preview) -> Option<Widening> {
        if self.leaves_machine && !planned.leaves_machine {
            return Some(Widening::LeavesMachine);
        }
        if let Some(d) = self.destinations.iter().find(|d| !planned.destinations.contains(d)) {
            return Some(Widening::destination(d.clone()));
        }
        if let Some(e) = &self.endpoint
            && planned.endpoint.as_ref() != Some(e)
        {
            return Some(Widening::endpoint(e.clone()));
        }
        for now in &self.outbound {
            let (count, bytes) = planned
                .outbound
                .iter()
                .filter(|o| o.what == now.what)
                .fold((0u32, 0u64), |(c, b), o| (c.saturating_add(o.count), b.saturating_add(o.bytes)));
            if now.count > count || now.bytes > bytes {
                return Some(Widening::outbound(now.what));
            }
        }
        if self.trains_on_inputs != planned.trains_on_inputs {
            return Some(Widening::TrainsOnInputs);
        }
        if self.retention != planned.retention {
            return Some(Widening::Retention);
        }
        if self.secret != planned.secret {
            return Some(Widening::Secret);
        }
        if self.needs_person.is_some() && planned.needs_person.is_none() {
            return Some(Widening::NeedsPerson);
        }
        if self.plan != planned.plan {
            return Some(Widening::Plan);
        }
        (self.estimate.usd_micros > planned.estimate.usd_micros)
            .then(|| Widening::estimate(planned.estimate.usd_micros, self.estimate.usd_micros))
    }
}

/// "a prompt", "3 images".
fn counted(what: Sends, n: u32) -> String {
    if n == 1 {
        let one = match what {
            Sends::Prompt => "a prompt",
            Sends::Images => "an image",
            Sends::Audio => "an audio clip",
            Sends::Meshes => "a mesh",
            Sends::Video => "a video",
            Sends::Files => "a file",
        };
        one.to_owned()
    } else {
        format!("{n} {}", sends_noun(what, n))
    }
}

fn sends_noun(what: Sends, n: u32) -> &'static str {
    match (what, n == 1) {
        (Sends::Prompt, true) => "prompt",
        (Sends::Prompt, false) => "prompts",
        (Sends::Images, true) => "image",
        (Sends::Images, false) => "images",
        (Sends::Audio, true) => "audio clip",
        (Sends::Audio, false) => "audio clips",
        (Sends::Meshes, true) => "mesh",
        (Sends::Meshes, false) => "meshes",
        (Sends::Video, true) => "video",
        (Sends::Video, false) => "videos",
        (Sends::Files, true) => "file",
        (Sends::Files, false) => "files",
    }
}

/// "a", "a and b", "a, b and c".
fn join_and(items: &[String]) -> String {
    match items {
        [] => String::new(),
        [one] => one.clone(),
        [rest @ .., last] => format!("{} and {last}", rest.join(", ")),
    }
}
