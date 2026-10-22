//! The connector manifest (D3): one `connectors/<id>.connector.toml` per connector, embedded with `include_str!` and
//! registered with [`register_connector!`](crate::register_connector), so the licence gate reads every shipped
//! manifest without building.
//!
//! [`ConnectorManifest::from_toml`] refuses unknown keys; [`ConnectorManifest::validate`] reports every broken rule
//! with its field path, not just the first.

use std::collections::{BTreeMap, BTreeSet};

use omuzaki_ai_api::Version;
use omuzaki_store_api::Commercial;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::id::{Capability, ConnectorId, MediaKind};
use crate::net::{Endpoint, Scheme, is_local_destination};
use crate::params::ParamDecl;
use crate::request::MAX_COUNT;

/// The longest connector name, in characters.
pub const MAX_NAME_CHARS: usize = 80;

/// How the connector reaches its service.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Transport {
    /// HTTP through the host's transport, which attaches the key.
    Http,
    /// An MCP server, launched by the host or reached over HTTP; the `[mcp]` table maps capabilities to tools.
    Mcp,
    /// Through the LLM seam (`omuzaki-llm-api`), which holds the endpoint, key and prices (D2).
    Llm,
    /// In this process or a program on this computer with no endpoint (a registry slot, the reverse bridge).
    Local,
    /// A test fake: no network at all.
    Fake,
}

/// The key a connector needs, if any. In TOML: `[auth] kind = "none"`, or `kind = "api_key"` with `secret` and
/// `deliver`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AuthSpec {
    /// No key.
    None,
    /// An API key from the OS keychain.
    #[non_exhaustive]
    ApiKey {
        /// The keychain label, exactly `connector/<id>`, with `deliver` `header` or `env`; absent with `provider`,
        /// whose key belongs to the selected `llm.providers` entry (D3). The host never reads this field directly:
        /// it uses the effective label [`Target::secret`](crate::Target::secret).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        secret: Option<String>,
        /// How the key reaches the service.
        deliver: Deliver,
    },
}

impl AuthSpec {
    /// [`AuthSpec::ApiKey`].
    pub fn api_key(secret: Option<String>, deliver: Deliver) -> Self {
        Self::ApiKey { secret, deliver }
    }
}

/// How a key reaches the service. In TOML: `deliver = { via = "header", name = "authorization", scheme = "Bearer" }`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "via", rename_all = "snake_case", deny_unknown_fields)]
pub enum Deliver {
    /// An HTTP header `<name>: <scheme> <key>`, attached by the host's transport; an empty scheme sends the bare key.
    #[non_exhaustive]
    Header {
        /// The header name: an HTTP token.
        name: String,
        /// The scheme before the key (`Bearer`), or empty.
        scheme: String,
    },
    /// An environment variable of a child process the host launches (MCP over stdio), never its arguments.
    #[non_exhaustive]
    Env {
        /// The variable name, `[A-Z][A-Z0-9_]*`.
        name: String,
    },
    /// The LLM seam's provider resolves its own key (D2); only for `transport = "llm"`.
    Provider,
}

impl Deliver {
    /// [`Deliver::Header`].
    pub fn header(name: impl Into<String>, scheme: impl Into<String>) -> Self {
        Self::Header { name: name.into(), scheme: scheme.into() }
    }

    /// [`Deliver::Env`].
    pub fn env(name: impl Into<String>) -> Self {
        Self::Env { name: name.into() }
    }
}

/// What one billable unit is.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Unit {
    /// One call.
    Call,
    /// One image.
    Image,
    /// One second of audio or motion.
    Second,
    /// A thousand characters of text, rounded up.
    ThousandChars,
    /// One mesh.
    Mesh,
}

/// What a call costs. Every price carries the date (`YYYY-MM-DD`) it was read from the vendor's price page: a price
/// list is never quoted from memory, and a stale date is visible in review. In TOML: `[cost] model = "per_unit"`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "model", rename_all = "snake_case", deny_unknown_fields)]
pub enum CostModel {
    /// Nothing.
    Free,
    /// A fixed price per call.
    #[non_exhaustive]
    PerCall {
        /// Micro-dollars per call.
        usd_micros: u64,
        /// When the price was read, `YYYY-MM-DD`.
        priced: String,
    },
    /// A price per unit.
    #[non_exhaustive]
    PerUnit {
        /// What one unit is.
        unit: Unit,
        /// Micro-dollars per unit.
        usd_micros: u64,
        /// When the price was read, `YYYY-MM-DD`.
        priced: String,
    },
    /// Prices per million tokens.
    #[non_exhaustive]
    PerToken {
        /// Micro-dollars per million input tokens.
        input_usd_micros_per_mtok: u64,
        /// Micro-dollars per million output tokens.
        output_usd_micros_per_mtok: u64,
        /// When the prices were read, `YYYY-MM-DD`.
        priced: String,
    },
    /// The LLM seam's dated price table, the one source (D2); only for `transport = "llm"`. The connector's
    /// [`estimate`](crate::Connector::estimate) asks the seam.
    Provider,
}

impl CostModel {
    /// [`CostModel::PerCall`].
    pub fn per_call(usd_micros: u64, priced: impl Into<String>) -> Self {
        Self::PerCall { usd_micros, priced: priced.into() }
    }

    /// [`CostModel::PerUnit`].
    pub fn per_unit(unit: Unit, usd_micros: u64, priced: impl Into<String>) -> Self {
        Self::PerUnit { unit, usd_micros, priced: priced.into() }
    }

    /// [`CostModel::PerToken`].
    pub fn per_token(
        input_usd_micros_per_mtok: u64,
        output_usd_micros_per_mtok: u64,
        priced: impl Into<String>,
    ) -> Self {
        Self::PerToken { input_usd_micros_per_mtok, output_usd_micros_per_mtok, priced: priced.into() }
    }

    /// The `priced` date of a model that has one.
    pub fn priced(&self) -> Option<&str> {
        match self {
            CostModel::PerCall { priced, .. }
            | CostModel::PerUnit { priced, .. }
            | CostModel::PerToken { priced, .. } => Some(priced),
            CostModel::Free | CostModel::Provider => None,
        }
    }
}

/// What a call sends out, by kind (the preview counts each).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Sends {
    /// The prompt or other text.
    Prompt,
    /// Images.
    Images,
    /// Audio.
    Audio,
    /// Meshes.
    Meshes,
    /// Video.
    Video,
    /// Other files (a motion file, a DCC program's input).
    Files,
}

/// A three-way answer from a vendor's terms.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Tri {
    /// No.
    No,
    /// Yes.
    Yes,
    /// The terms don't say.
    Unknown,
}

/// What leaves the computer and where it goes, as the manifest declares it. The effective destination can change
/// with the endpoint a person sets ([`Target`](crate::Target)).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PrivacyNotes {
    /// Whether anything is sent off this computer.
    pub leaves_machine: bool,
    /// Hosts, or `stdio` for a program the host launches.
    pub destinations: Vec<String>,
    /// What kinds of data a call sends.
    #[serde(default)]
    pub sends: Vec<Sends>,
    /// Words from the vendor's terms, shown in the preview.
    #[serde(default)]
    pub retention: String,
    /// Whether the vendor may train on what it is sent.
    pub trains_on_inputs: Tri,
}

/// Where the licence of outputs comes from (D6).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum OutputsFrom {
    /// The vendor's terms: the manifest's `spdx` and `commercial`.
    VendorTerms,
    /// The person's declaration for the model a local program runs (ADR 0016).
    UserModel,
    /// The adapter's report of the model it ran ([`Artifact::rights`](crate::Artifact::rights)).
    Reported,
}

/// The licence policy for outputs (D6).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct LicencePolicy {
    /// The `item` of this connector's licence file under `docs/licensing/deps/`.
    pub register: String,
    /// Where the outputs' licence comes from.
    pub outputs: OutputsFrom,
    /// What provenance records as the licence of outputs (SPDX or `LicenseRef-...`).
    pub spdx: String,
    /// The default; the host only ever narrows it. `unknown` for `user_model` and `reported`.
    pub commercial: Commercial,
    /// Plan tiers allowed to produce shippable output; empty means no plan rule.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requires_plan: Vec<String>,
    /// Attribution text that must travel with outputs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attribution: Option<String>,
}

/// One capability a connector offers.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CapabilityDecl {
    /// The capability.
    pub capability: Capability,
    /// What the request may carry.
    #[serde(default)]
    pub inputs: Vec<MediaKind>,
    /// What the outputs are; each must fit the capability.
    pub outputs: Vec<MediaKind>,
    /// The most outputs one call may ask for, 1 to [`MAX_COUNT`].
    pub max_count: u8,
    /// The adapter knobs a request may set (D2).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub params: Vec<ParamDecl>,
}

/// Where one argument of an MCP tool comes from. In TOML: `{ field = "prompt" }` or `{ const = 4 }`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ArgSource {
    /// A field of the request, by name.
    Field(String),
    /// A constant.
    Const(Value),
}

/// For [`Transport::Mcp`]: which tool serves a capability and where each argument comes from.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct McpTool {
    /// The capability it serves.
    pub capability: Capability,
    /// The tool's name on the server. For `dcc`, the request's `action`.
    pub tool: String,
    /// Each argument and its source.
    #[serde(default)]
    pub args: BTreeMap<String, ArgSource>,
    /// A key of a JSON text result that holds a download URL (a service that returns a link, not the file).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url_field: Option<String>,
}

/// The `[mcp]` table: the tool map one `McpConnector` serves every MCP vendor with.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct McpSpec {
    /// The tools, at least one per declared capability; several for `dcc`, one per action.
    pub tools: Vec<McpTool>,
}

/// A connector, as its `<id>.connector.toml` declares it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ConnectorManifest {
    /// The id.
    pub id: ConnectorId,
    /// The name people see, 1 to [`MAX_NAME_CHARS`] characters.
    pub name: String,
    /// The manifest's version; the tray records it and replay compares it (D10).
    pub version: Version,
    /// How it talks.
    pub transport: Transport,
    /// What it offers.
    pub capabilities: Vec<CapabilityDecl>,
    /// Its key.
    pub auth: AuthSpec,
    /// What it costs.
    pub cost: CostModel,
    /// What leaves the computer.
    pub privacy: PrivacyNotes,
    /// The licence of its outputs.
    pub licence: LicencePolicy,
    /// The MCP tool map, for and only for [`Transport::Mcp`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp: Option<McpSpec>,
    /// The default base URL of an HTTP connector (a person overrides it in settings): `https://`, or `http://` with a
    /// loopback host. Its host is one of `destinations`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint: Option<String>,
}

/// Why a manifest was refused.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ManifestError {
    /// The TOML does not read: a syntax error, a wrong type or an unknown key.
    #[error("the manifest does not read: {0}")]
    Parse(String),
    /// A field breaks a rule.
    #[error("{field}: {problem}")]
    #[non_exhaustive]
    Field {
        /// The field path, `licence.commercial` or `capabilities.1.max_count`.
        field: String,
        /// What is wrong, and what to write instead.
        problem: String,
    },
}

impl ManifestError {
    /// [`ManifestError::Field`].
    pub fn field(field: impl Into<String>, problem: impl Into<String>) -> Self {
        Self::Field { field: field.into(), problem: problem.into() }
    }
}

impl ConnectorManifest {
    /// Reads a manifest. Unknown keys are an error; the rules are [`validate`](Self::validate)'s.
    ///
    /// # Errors
    /// [`ManifestError::Parse`] with the TOML reader's message.
    pub fn from_toml(text: &str) -> Result<Self, ManifestError> {
        toml::from_str(text).map_err(|e| ManifestError::Parse(e.to_string()))
    }

    /// The manifest as TOML.
    ///
    /// # Errors
    /// [`ManifestError::Parse`] if a value has no TOML form (a `null` parameter default).
    pub fn to_toml(&self) -> Result<String, ManifestError> {
        toml::to_string(self).map_err(|e| ManifestError::Parse(e.to_string()))
    }

    /// The declaration of `c`, if this connector offers it.
    pub fn declares(&self, c: Capability) -> Option<&CapabilityDecl> {
        self.capabilities.iter().find(|d| d.capability == c)
    }

    /// True unless the cost model is [`CostModel::Free`].
    pub fn paid(&self) -> bool {
        self.cost != CostModel::Free
    }

    /// Checks every rule (D3).
    ///
    /// # Errors
    /// Every [`ManifestError::Field`] found, in field order, not just the first.
    pub fn validate(&self) -> Result<(), Vec<ManifestError>> {
        let mut found = Vec::new();
        let mut bad = |field: &str, problem: String| found.push(ManifestError::field(field, problem));
        self.check_name_and_capabilities(&mut bad);
        self.check_auth(&mut bad);
        self.check_cost(&mut bad);
        self.check_privacy(&mut bad);
        self.check_licence(&mut bad);
        self.check_endpoint(&mut bad);
        self.check_mcp(&mut bad);
        if found.is_empty() { Ok(()) } else { Err(found) }
    }

    fn check_name_and_capabilities(&self, bad: &mut impl FnMut(&str, String)) {
        let chars = self.name.chars().count();
        if chars == 0 || chars > MAX_NAME_CHARS {
            bad("name", format!("a name is 1 to {MAX_NAME_CHARS} characters; this one has {chars}"));
        }
        if self.capabilities.is_empty() {
            bad("capabilities", "declare at least one capability".to_owned());
        }
        let mut seen = BTreeSet::new();
        for (i, decl) in self.capabilities.iter().enumerate() {
            let at = format!("capabilities.{i}");
            if !seen.insert(decl.capability) {
                bad(&format!("{at}.capability"), format!("{} is declared twice", decl.capability));
            }
            if decl.max_count == 0 || decl.max_count > MAX_COUNT {
                bad(&format!("{at}.max_count"), format!("max_count is 1 to {MAX_COUNT}, not {}", decl.max_count));
            }
            if decl.outputs.is_empty() {
                bad(&format!("{at}.outputs"), "declare at least one output kind".to_owned());
            }
            for (j, kind) in decl.outputs.iter().enumerate() {
                if !decl.capability.allows_output(*kind) {
                    bad(&format!("{at}.outputs.{j}"), format!("{} cannot give {kind} outputs", decl.capability));
                }
            }
            check_param_decls(&at, &decl.params, bad);
        }
    }

    fn check_auth(&self, bad: &mut impl FnMut(&str, String)) {
        let AuthSpec::ApiKey { secret, deliver } = &self.auth else {
            return;
        };
        let label = format!("connector/{}", self.id);
        match (deliver, secret) {
            (Deliver::Provider, Some(_)) => bad(
                "auth.secret",
                "a provider-delivered key belongs to the selected llm.providers entry; remove `secret`".to_owned(),
            ),
            (Deliver::Header { .. } | Deliver::Env { .. }, None) => {
                bad("auth.secret", format!("an api key needs `secret = \"{label}\"`"));
            }
            (Deliver::Header { .. } | Deliver::Env { .. }, Some(s)) if *s != label => {
                bad("auth.secret", format!("the keychain label is exactly \"{label}\", not {s:?}"));
            }
            _ => {}
        }
        match deliver {
            Deliver::Header { name, scheme } => {
                if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
                    bad("auth.deliver.name", format!("{name:?} is not a header name (letters, digits and '-')"));
                }
                if scheme.contains(char::is_whitespace) {
                    bad("auth.deliver.scheme", format!("a scheme has no spaces, not {scheme:?}"));
                }
                if self.transport == Transport::Mcp {
                    bad("auth.deliver", "an MCP connector gets its key in its environment: use `via = \"env\"`".into());
                }
            }
            Deliver::Env { name } => {
                let mut chars = name.chars();
                let ok = chars.next().is_some_and(|c| c.is_ascii_uppercase())
                    && chars.all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_');
                if !ok {
                    bad("auth.deliver.name", format!("{name:?} is not an environment name ([A-Z][A-Z0-9_]*)"));
                }
                if self.transport == Transport::Http {
                    bad("auth.deliver", "an HTTP connector sends its key in a header: use `via = \"header\"`".into());
                }
            }
            Deliver::Provider => {
                if self.transport != Transport::Llm {
                    bad("auth.deliver", "only an `llm` transport gets its key from the provider".to_owned());
                }
            }
        }
    }

    fn check_cost(&self, bad: &mut impl FnMut(&str, String)) {
        if let Some(priced) = self.cost.priced()
            && !is_date(priced)
        {
            bad("cost.priced", format!("a price carries the date it was read, YYYY-MM-DD, not {priced:?}"));
        }
        let llm = self.transport == Transport::Llm;
        if llm && !matches!(self.cost, CostModel::Free | CostModel::Provider) {
            bad("cost.model", "an `llm` connector prices from the seam's table: use `provider` or `free`".to_owned());
        }
        if !llm && self.cost == CostModel::Provider {
            bad("cost.model", "only an `llm` transport takes its prices from the provider".to_owned());
        }
        if llm {
            if let AuthSpec::ApiKey { deliver, .. } = &self.auth
                && *deliver != Deliver::Provider
            {
                bad("auth.deliver", "an `llm` connector's key is the provider's: use `via = \"provider\"`".to_owned());
            }
            for (i, decl) in self.capabilities.iter().enumerate() {
                if !matches!(decl.capability, Capability::Llm | Capability::Vision) {
                    bad(
                        &format!("capabilities.{i}.capability"),
                        "an `llm` connector offers only llm and vision".into(),
                    );
                }
            }
        }
    }

    fn check_privacy(&self, bad: &mut impl FnMut(&str, String)) {
        let p = &self.privacy;
        if p.leaves_machine {
            if p.destinations.is_empty() {
                bad("privacy.destinations", "a connector that leaves the machine names where it goes".to_owned());
            }
            for (i, d) in p.destinations.iter().enumerate() {
                if is_local_destination(d) {
                    bad(
                        &format!("privacy.destinations.{i}"),
                        format!("{d} stays on this computer, yet leaves_machine is true"),
                    );
                }
            }
            if p.retention.trim().is_empty() {
                bad("privacy.retention", "say what the vendor keeps and for how long, in its terms' words".to_owned());
            }
        } else {
            for (i, d) in p.destinations.iter().enumerate() {
                if !is_local_destination(d) {
                    bad(
                        &format!("privacy.destinations.{i}"),
                        format!("{d} is off this computer, yet leaves_machine is false"),
                    );
                }
            }
        }
    }

    fn check_licence(&self, bad: &mut impl FnMut(&str, String)) {
        let l = &self.licence;
        if l.register.trim().is_empty() {
            bad("licence.register", "name the item of this connector's licence file".to_owned());
        }
        if l.spdx.trim().is_empty() {
            bad("licence.spdx", "an SPDX expression or LicenseRef-... is required".to_owned());
        }
        if l.outputs != OutputsFrom::VendorTerms && l.commercial != Commercial::Unknown {
            bad(
                "licence.commercial",
                "a user's or reported model decides its own outputs' licence: write `commercial = \"unknown\"`".into(),
            );
        }
        for (i, plan) in l.requires_plan.iter().enumerate() {
            if omuzaki_core::id::Slug::new(plan.as_str()).is_err() {
                bad(&format!("licence.requires_plan.{i}"), format!("{plan:?} is not a slug"));
            }
        }
    }

    fn check_endpoint(&self, bad: &mut impl FnMut(&str, String)) {
        match (self.transport, &self.endpoint) {
            (Transport::Http, None) => bad("endpoint", "an HTTP connector needs a default endpoint".to_owned()),
            (Transport::Http, Some(url)) => match Endpoint::parse(url) {
                None => bad("endpoint", format!("{url:?} is not an http:// or https:// address without user-info")),
                Some(e) => {
                    if e.scheme == Scheme::Http && !e.is_loopback() {
                        bad("endpoint", format!("a shipped endpoint off this computer is https://, not {url:?}"));
                    }
                    if !self.privacy.destinations.iter().any(|d| d.eq_ignore_ascii_case(&e.host)) {
                        bad("endpoint", format!("its host {} is not one of privacy.destinations", e.host));
                    }
                }
            },
            (_, Some(_)) => {
                bad("endpoint", format!("only an HTTP connector has an endpoint, not {:?}", self.transport));
            }
            (_, None) => {}
        }
    }

    fn check_mcp(&self, bad: &mut impl FnMut(&str, String)) {
        let Some(mcp) = &self.mcp else {
            if self.transport == Transport::Mcp {
                bad("mcp", "an MCP connector needs an [mcp] tool map".to_owned());
            }
            return;
        };
        if self.transport != Transport::Mcp {
            bad("mcp", "only an MCP connector has an [mcp] table".to_owned());
            return;
        }
        let mut seen = BTreeSet::new();
        for (i, tool) in mcp.tools.iter().enumerate() {
            if self.declares(tool.capability).is_none() {
                bad(&format!("mcp.tools.{i}.capability"), format!("{} is not declared", tool.capability));
            }
            if !seen.insert((tool.capability, tool.tool.as_str())) {
                bad(&format!("mcp.tools.{i}.tool"), format!("{} {} is mapped twice", tool.capability, tool.tool));
            } else if tool.capability != Capability::Dcc
                && mcp.tools[..i].iter().any(|t| t.capability == tool.capability)
            {
                bad(&format!("mcp.tools.{i}"), format!("{} has one tool; only dcc maps several", tool.capability));
            }
        }
        for decl in &self.capabilities {
            if !mcp.tools.iter().any(|t| t.capability == decl.capability) {
                bad("mcp.tools", format!("no tool serves {}", decl.capability));
            }
        }
    }
}

/// Checks one capability's param declarations: unique slug names, a valid kind, a default of that kind.
fn check_param_decls(at: &str, params: &[ParamDecl], bad: &mut impl FnMut(&str, String)) {
    let mut names = BTreeSet::new();
    for (k, p) in params.iter().enumerate() {
        let here = format!("{at}.params.{k}");
        if omuzaki_core::id::Slug::new(p.name.as_str()).is_err() {
            bad(&format!("{here}.name"), format!("{:?} is not a slug", p.name));
        } else if !names.insert(p.name.as_str()) {
            bad(&format!("{here}.name"), format!("{} is declared twice", p.name));
        }
        if let Some(problem) = p.kind.problem() {
            bad(&format!("{here}.kind"), problem);
        } else if let Some(problem) = p.default.as_ref().and_then(|d| p.kind.reject(d)) {
            bad(&format!("{here}.default"), problem);
        }
    }
}

/// Whether `s` is a `YYYY-MM-DD` date with a month 01 to 12 and a day 01 to 31.
fn is_date(s: &str) -> bool {
    let b = s.as_bytes();
    let digits = |r: std::ops::Range<usize>| b[r.clone()].iter().all(u8::is_ascii_digit).then(|| &s[r]);
    if b.len() != 10 || b[4] != b'-' || b[7] != b'-' {
        return false;
    }
    let month = digits(5..7).and_then(|m| m.parse::<u8>().ok());
    let day = digits(8..10).and_then(|d| d.parse::<u8>().ok());
    digits(0..4).is_some() && month.is_some_and(|m| (1..=12).contains(&m)) && day.is_some_and(|d| (1..=31).contains(&d))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dates_are_checked() {
        assert!(is_date("2026-10-13"));
        for bad in ["2026-13-01", "2026-00-10", "2026-1-01", "26-10-13", "2026/10/13", "2026-10-32", "abcd-10-13"] {
            assert!(!is_date(bad), "{bad}");
        }
    }
}
