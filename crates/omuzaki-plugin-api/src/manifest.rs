//! The plugin manifest, `plugin.toml` (design `plugins.md` §7): its fields, the parser and the checks.
//!
//! [`Manifest::parse`] refuses a file whose shape is wrong (an unknown key, a scope that does not exist, a value of
//! the wrong type) and names the path of the bad value. [`Manifest::check`] then reports every rule the content breaks,
//! each with a stable id `plugin.<rule>`, so the installer, the store and the manager panel all refuse or warn for the
//! same reasons.

use std::fmt;
use std::net::IpAddr;

use omuzaki_ops_api::Scope;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::PluginId;

/// The base world every plugin targets (`omuzaki:plugin`'s `plugin`).
pub const BASE_WORLD: &str = "plugin";

/// A world a plugin may name in `worlds`, registered with [`register_world!`](crate::register_world) where the
/// world is defined (ADR 0054: added, not edited in).
#[derive(Debug)]
pub struct WorldRegistration {
    /// The world's name in `omuzaki:plugin`.
    pub name: &'static str,
}

inventory::collect!(WorldRegistration);

/// Registers a world a plugin may name in `worlds`: `omuzaki_plugin_api::register_world!("node");` in the crate that
/// serves it. The registration is recorded for the link gate (ADR 0015 §3), and the crate needs no `inventory` of its
/// own.
#[macro_export]
macro_rules! register_world {
    ($name:literal) => {
        $crate::__inventory::submit! { $crate::manifest::WorldRegistration { name: $name } }
        $crate::__record_registration!("world", $name);
    };
}

crate::register_world!("plugin");

/// Every registered world, in link order.
pub fn known_worlds() -> impl Iterator<Item = &'static str> {
    inventory::iter::<WorldRegistration>.into_iter().map(|w| w.name)
}

/// Ids a plugin may not take: they name Omuzaki itself. The first-party op families are checked at install, against
/// the op registry.
pub const RESERVED_IDS: [&str; 4] = ["omuzaki", "core", "plugin", "system"];

/// The data quota when the manifest names none, in MiB.
pub const DEFAULT_DATA_QUOTA_MB: u32 = 64;

/// The largest data quota a manifest may ask for, in MiB.
pub const MAX_DATA_QUOTA_MB: u32 = 1024;

/// The most settings one manifest may declare.
pub const MAX_SETTINGS: usize = 64;

/// The longest setting key, in characters.
pub const MAX_SETTING_KEY_LEN: usize = 32;

/// The scopes a plugin is never granted. `native` has its own rule id; the others share `plugin.scope_refused`.
pub const NEVER_GRANTED: [Scope; 5] = [Scope::Native, Scope::Fs, Scope::Process, Scope::Input, Scope::Script];

/// A parsed `plugin.toml`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    /// The plugin's id: a slug, `^[a-z][a-z0-9_]{1,31}$`, not a reserved word.
    pub id: PluginId,
    /// The plugin's own version, semver.
    pub version: String,
    /// The WIT versions it was built for, a semver requirement (`"^0.1"`); it must agree with the component's
    /// imports.
    pub api: String,
    /// Its licence, an SPDX expression.
    pub licence: String,
    /// Its display name, the fallback for the Fluent `plugin-<id>-name`.
    pub name: String,
    /// One line about what it does.
    #[serde(default)]
    pub description: String,
    /// The worlds its component targets: the base world, plus PL-03's extension worlds.
    #[serde(default = "base_worlds")]
    pub worlds: Vec<String>,
    /// The component file, a normalised path relative to the package root.
    pub component: String,
    /// Who publishes it; an unsigned plugin omits it.
    #[serde(default)]
    pub publisher: Option<Publisher>,
    /// What it asks for.
    #[serde(default)]
    pub requests: Requests,
    /// Its settings, registered under `plugin.<id>.*` while it is enabled.
    #[serde(default)]
    pub settings: Vec<SettingDecl>,
}

fn base_worlds() -> Vec<String> {
    vec![BASE_WORLD.to_owned()]
}

/// Who publishes a plugin. The key binds nothing by itself: the package signature does (ADR 0042).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Publisher {
    /// The publisher's display name.
    pub name: String,
    /// `ed25519:` and the key's fingerprint (16 lower-case hex digits) or the whole key (64).
    pub key: String,
}

/// What a plugin asks for. The person approves a subset; the ceiling is the intersection (design §3.2).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Requests {
    /// The op scopes it wants. `net` means "may use `http.fetch`", to `net_hosts` only.
    #[serde(default)]
    pub scopes: Vec<Scope>,
    /// The exact hosts `fetch` may reach (`example.com`, `api.example.com:8443`, `localhost:8080`).
    #[serde(default)]
    pub net_hosts: Vec<String>,
    /// Whether it wants its own data folder.
    #[serde(default)]
    pub data: bool,
    /// The data folder's quota in MiB, 1 to [`MAX_DATA_QUOTA_MB`].
    #[serde(default = "default_quota")]
    pub data_quota_mb: u32,
}

fn default_quota() -> u32 {
    DEFAULT_DATA_QUOTA_MB
}

impl Default for Requests {
    fn default() -> Self {
        Self { scopes: Vec::new(), net_hosts: Vec::new(), data: false, data_quota_mb: DEFAULT_DATA_QUOTA_MB }
    }
}

/// One setting a plugin declares (design §7, `settings.md` D2).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SettingDecl {
    /// The key under `plugin.<id>.`: `^[a-z][a-z0-9_]{0,31}$`.
    pub key: String,
    /// A simple type; exactly one of `type` and `schema` is given.
    #[serde(rename = "type", default)]
    pub kind: Option<SettingType>,
    /// A JSON Schema, for a value no simple type describes.
    #[serde(default)]
    pub schema: Option<Value>,
    /// The default value; it must have the declared type.
    pub default: Value,
    /// Where the value is kept.
    #[serde(default)]
    pub scope: SettingScope,
    /// Whether an agent may change it.
    #[serde(default)]
    pub agent: AgentRule,
}

/// A setting's simple type.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SettingType {
    /// Text.
    String,
    /// On or off.
    Bool,
    /// A whole number.
    Integer,
    /// Any number.
    Number,
}

impl SettingType {
    /// Whether `value` has this type.
    pub fn admits(self, value: &Value) -> bool {
        match self {
            Self::String => value.is_string(),
            Self::Bool => value.is_boolean(),
            Self::Integer => value.as_number().is_some_and(crate::setting_schema::is_whole),
            Self::Number => value.is_number(),
        }
    }
}

/// Where a setting's value is kept (`settings.md` D2).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SettingScope {
    /// The person's settings, which roam with them.
    #[default]
    User,
    /// This machine's settings.
    Machine,
}

/// Whether an agent may change a setting (`settings.md` D4).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AgentRule {
    /// Agents may change it.
    Free,
    /// Agents may change it once the person confirms.
    Confirm,
    /// Only the person changes it.
    #[default]
    Never,
}

/// Why `plugin.toml` could not be read: the path of the bad value (`requests/scopes/2`, empty for the whole file) and
/// what was wrong with it.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("plugin.toml{}{path}: {message}", if path.is_empty() { "" } else { " at " })]
pub struct ManifestError {
    /// Keys and indices from the root to the bad value, joined by `/`.
    pub path: String,
    /// What was wrong.
    pub message: String,
}

/// How serious a finding is.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    /// The plugin is refused.
    Error,
    /// The person is told; nothing is refused.
    Warning,
}

/// One rule a manifest breaks.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Finding {
    /// The rule's stable id, `plugin.<rule>`.
    pub id: String,
    /// Whether it refuses the plugin.
    pub severity: Severity,
    /// The path of the value at fault, as in [`ManifestError::path`].
    pub path: String,
    /// What is wrong, in English, for logs and the CLI; surfaces render the id.
    pub message: String,
}

/// Every finding of [`Manifest::check`], in the order the fields are declared.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Report {
    /// The findings.
    pub findings: Vec<Finding>,
}

impl Report {
    /// The findings that refuse the plugin.
    pub fn errors(&self) -> impl Iterator<Item = &Finding> {
        self.findings.iter().filter(|f| f.severity == Severity::Error)
    }

    /// The findings the person is only told about.
    pub fn warnings(&self) -> impl Iterator<Item = &Finding> {
        self.findings.iter().filter(|f| f.severity == Severity::Warning)
    }

    /// Whether any finding refuses the plugin.
    pub fn has_errors(&self) -> bool {
        self.errors().next().is_some()
    }

    /// Whether there is no finding at all.
    pub fn is_clean(&self) -> bool {
        self.findings.is_empty()
    }

    fn error(&mut self, id: &str, path: impl Into<String>, message: impl Into<String>) {
        self.push(id, Severity::Error, path, message);
    }

    fn warning(&mut self, id: &str, path: impl Into<String>, message: impl Into<String>) {
        self.push(id, Severity::Warning, path, message);
    }

    fn push(&mut self, id: &str, severity: Severity, path: impl Into<String>, message: impl Into<String>) {
        self.findings.push(Finding { id: id.to_owned(), severity, path: path.into(), message: message.into() });
    }
}

impl fmt::Display for Finding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({}): {}", self.id, self.path, self.message)
    }
}

/// What a licence lets a person do with what the plugin makes (ADR 0016's pattern; `docs/licensing/policy.md`).
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LicenceClass {
    /// A permissive licence the policy allows.
    Allowed,
    /// Allowed with conditions (copyleft, attribution, notices).
    Conditions,
    /// Not one the table knows: shown as unknown.
    Unknown,
    /// Non-commercial or restricted source-available: shown with a warning; the store refuses it.
    NonCommercial,
}

/// Permissive licences, from the policy's allow list (`deny.toml`).
const ALLOWED_LICENCES: [&str; 14] = [
    "MIT",
    "MIT-0",
    "Apache-2.0",
    "BSD-2-Clause",
    "BSD-3-Clause",
    "ISC",
    "Zlib",
    "BSL-1.0",
    "0BSD",
    "Unlicense",
    "CC0-1.0",
    "Unicode-3.0",
    "Unicode-DFS-2016",
    "MPL-2.0",
];

/// Licences that are fine with conditions (attribution, notices, copyleft). Copyleft binds whoever distributes the plugin, which Omuzaki does not, so
/// it is shown as a condition rather than refused (design §8).
const CONDITIONS_LICENCES: [&str; 14] = [
    "LGPL-2.1-only",
    "LGPL-2.1-or-later",
    "LGPL-3.0-only",
    "LGPL-3.0-or-later",
    "GPL-2.0-only",
    "GPL-2.0-or-later",
    "GPL-3.0-only",
    "GPL-3.0-or-later",
    "AGPL-3.0-only",
    "AGPL-3.0-or-later",
    "EPL-2.0",
    "OFL-1.1",
    "CC-BY-4.0",
    "CC-BY-SA-4.0",
];

/// Restricted families, matched as prefixes: the policy bans them.
const RESTRICTED_PREFIXES: [&str; 6] =
    ["PolyForm-Noncommercial", "BUSL-1.1", "SSPL-1.0", "Elastic-2.0", "Commons-Clause", "LicenseRef-Commons-Clause"];

/// SPDX exceptions that only add permissions to their base licence, so `<licence> WITH <exception>` keeps the base's
/// class. Any other exception may take permissions away: unknown, or restricted when its id is a restricted family.
const PERMISSIVE_EXCEPTIONS: [&str; 8] = [
    "LLVM-exception",
    "Classpath-exception-2.0",
    "GCC-exception-3.1",
    "GCC-exception-2.0",
    "Autoconf-exception-3.0",
    "Bison-exception-2.2",
    "Font-exception-2.0",
    "OCaml-LGPL-linking-exception",
];

/// The marker of a non-commercial Creative Commons or similar id (`CC-BY-NC-4.0`).
const NON_COMMERCIAL_MARK: &str = "-NC";

/// The class of one SPDX licence id.
fn licence_id_class(id: &str) -> LicenceClass {
    let id = id.strip_suffix('+').unwrap_or(id);
    if id.contains(NON_COMMERCIAL_MARK) || RESTRICTED_PREFIXES.iter().any(|p| id.starts_with(p)) {
        LicenceClass::NonCommercial
    } else if ALLOWED_LICENCES.contains(&id) {
        LicenceClass::Allowed
    } else if CONDITIONS_LICENCES.contains(&id) {
        LicenceClass::Conditions
    } else {
        LicenceClass::Unknown
    }
}

/// The class an exception that is not one of [`PERMISSIVE_EXCEPTIONS`] gives: restricted for a restricted family's
/// id, else unknown.
fn exception_class(exception: &str) -> LicenceClass {
    if RESTRICTED_PREFIXES.iter().any(|p| exception.starts_with(p)) {
        LicenceClass::NonCommercial
    } else {
        LicenceClass::Unknown
    }
}

/// The class of an SPDX expression: `OR` takes the best side, `AND` the worst, `WITH` the base licence when the
/// exception is one of [`PERMISSIVE_EXCEPTIONS`] and the worse of the base and the exception's own class otherwise,
/// parentheses group. A malformed expression is [`LicenceClass::Unknown`].
pub fn licence_class(expression: &str) -> LicenceClass {
    let spaced = expression.replace('(', " ( ").replace(')', " ) ");
    let tokens: Vec<&str> = spaced.split_whitespace().collect();
    let mut parser = Spdx { tokens: &tokens, at: 0 };
    match parser.or() {
        Some(class) if parser.at == tokens.len() => class,
        _ => LicenceClass::Unknown,
    }
}

/// A recursive-descent reader of SPDX expressions, just enough to classify one.
struct Spdx<'a> {
    tokens: &'a [&'a str],
    at: usize,
}

impl<'a> Spdx<'a> {
    fn peek(&self) -> Option<&'a str> {
        self.tokens.get(self.at).copied()
    }

    fn or(&mut self) -> Option<LicenceClass> {
        let mut class = self.and()?;
        while self.peek() == Some("OR") {
            self.at += 1;
            class = class.min(self.and()?);
        }
        Some(class)
    }

    fn and(&mut self) -> Option<LicenceClass> {
        let mut class = self.with()?;
        while self.peek() == Some("AND") {
            self.at += 1;
            class = class.max(self.with()?);
        }
        Some(class)
    }

    fn with(&mut self) -> Option<LicenceClass> {
        let class = self.atom()?;
        if self.peek() == Some("WITH") {
            self.at += 1;
            let exception = self.peek()?;
            if matches!(exception, "(" | ")" | "AND" | "OR" | "WITH") {
                return None;
            }
            self.at += 1;
            if !PERMISSIVE_EXCEPTIONS.contains(&exception) {
                return Some(class.max(exception_class(exception)));
            }
        }
        Some(class)
    }

    fn atom(&mut self) -> Option<LicenceClass> {
        let token = self.peek()?;
        self.at += 1;
        match token {
            "(" => {
                let class = self.or()?;
                (self.peek() == Some(")")).then(|| self.at += 1)?;
                Some(class)
            }
            ")" | "AND" | "OR" | "WITH" => None,
            id => Some(licence_id_class(id)),
        }
    }
}

/// Why a component path is refused; `None` when it is a normalised relative path (design §7): no root, no drive
/// prefix, no backslash, no `.` or `..` component and no empty segment. Package staging then also requires it to be a
/// regular file inside the staged package.
pub fn component_path_problem(path: &str) -> Option<&'static str> {
    if path.is_empty() {
        Some("is empty")
    } else if path.starts_with('/') {
        Some("is absolute")
    } else if path.contains('\\') {
        Some("uses `\\`; package paths use `/`")
    } else if path.as_bytes().get(1) == Some(&b':') {
        Some("has a drive prefix")
    } else if path.split('/').any(str::is_empty) {
        Some("has an empty segment")
    } else if path.split('/').any(|segment| segment == "." || segment == "..") {
        Some("has a `.` or `..` component")
    } else {
        None
    }
}

/// Why a `net_hosts` entry is refused, or `None`: an exact lower-case host name with an optional port, no scheme,
/// path or wildcard, and no IP literal except `localhost:<port>` (design §3.3).
pub fn net_host_problem(entry: &str) -> Option<&'static str> {
    if entry.contains("://") {
        return Some("has a scheme");
    }
    if entry.contains('*') {
        return Some("is a wildcard");
    }
    if entry.contains('/') {
        return Some("has a path");
    }
    if entry.starts_with('[') || entry.parse::<IpAddr>().is_ok() {
        return Some("is an IP literal");
    }
    let (host, port) = match entry.split_once(':') {
        Some((host, port)) => (host, Some(port)),
        None => (entry, None),
    };
    if host.parse::<IpAddr>().is_ok() {
        return Some("is an IP literal");
    }
    if port.is_some_and(|port| port.parse::<u16>().map_or(true, |p| p == 0)) {
        return Some("has a port that is not 1 to 65535");
    }
    if host == "localhost" {
        return if port.is_some() { None } else { Some("is `localhost` without a port") };
    }
    let label_ok = |label: &str| {
        (1..=63).contains(&label.len())
            && label.bytes().all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
            && !label.starts_with('-')
            && !label.ends_with('-')
    };
    if host.len() > 253 || !host.split('.').all(label_ok) {
        return Some("is not a lower-case host name");
    }
    None
}

/// Whether `key` is a setting key: `^[a-z][a-z0-9_]{0,31}$`.
fn is_setting_key(key: &str) -> bool {
    let mut bytes = key.bytes();
    bytes.next().is_some_and(|b| b.is_ascii_lowercase())
        && bytes.all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
        && key.len() <= MAX_SETTING_KEY_LEN
}

/// Whether `key` is a publisher key: `ed25519:` and 16 or 64 lower-case hex digits.
fn is_publisher_key(key: &str) -> bool {
    key.strip_prefix("ed25519:").is_some_and(|hex| {
        matches!(hex.len(), 16 | 64) && hex.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    })
}

impl Manifest {
    /// Reads `plugin.toml`. Unknown keys are refused, so a typo can't silently drop a request.
    ///
    /// # Errors
    /// [`ManifestError`] naming the path of the first bad value.
    pub fn parse(text: &str) -> Result<Manifest, ManifestError> {
        let deserializer = toml::Deserializer::parse(text)
            .map_err(|e| ManifestError { path: String::new(), message: e.message().to_owned() })?;
        serde_path_to_error::deserialize(deserializer).map_err(|e| {
            let mut segments: Vec<String> =
                e.path()
                    .iter()
                    .filter_map(|segment| match segment {
                        serde_path_to_error::Segment::Seq { index } => Some(index.to_string()),
                        serde_path_to_error::Segment::Map { key }
                        | serde_path_to_error::Segment::Enum { variant: key } => Some(key.clone()),
                        serde_path_to_error::Segment::Unknown => None,
                    })
                    .collect();
            let message = e.inner().message().to_owned();
            // A missing or unknown key may be reported at its parent; name the key itself, once.
            let key = quoted_key(&message, "missing field `").or_else(|| quoted_key(&message, "unknown field `"));
            if let Some(key) = key.filter(|key| segments.last() != Some(key)) {
                segments.push(key);
            }
            ManifestError { path: segments.join("/"), message }
        })
    }

    /// Every rule the manifest breaks: errors refuse the plugin, warnings are shown.
    pub fn check(&self) -> Report {
        let mut report = Report::default();
        if RESERVED_IDS.contains(&self.id.as_str()) {
            report.error("plugin.id_reserved", "id", format!("`{}` is reserved for Omuzaki", self.id));
        }
        if semver::Version::parse(&self.version).is_err() {
            report.error("plugin.version", "version", format!("`{}` is not a semver version", self.version));
        }
        if semver::VersionReq::parse(&self.api).is_err() {
            report.error("plugin.api", "api", format!("`{}` is not a semver requirement", self.api));
        }
        self.check_licence(&mut report);
        if self.name.trim().is_empty() {
            report.error("plugin.name_missing", "name", "the plugin needs a name");
        }
        self.check_worlds(&mut report);
        if let Some(problem) = component_path_problem(&self.component) {
            report.error("plugin.component_path", "component", format!("`{}` {problem}", self.component));
        }
        match &self.publisher {
            None => report.warning("plugin.unsigned", "publisher", "no publisher: the plugin is unsigned"),
            Some(publisher) if !is_publisher_key(&publisher.key) => report.error(
                "plugin.publisher_key",
                "publisher/key",
                "the key is `ed25519:` and 16 or 64 lower-case hex digits",
            ),
            Some(_) => {}
        }
        self.check_requests(&mut report);
        self.check_settings(&mut report);
        report
    }

    /// The class of the manifest's licence expression.
    pub fn licence_class(&self) -> LicenceClass {
        licence_class(&self.licence)
    }

    fn check_licence(&self, report: &mut Report) {
        if self.licence.trim().is_empty() {
            report.error("plugin.licence_missing", "licence", "the plugin needs an SPDX licence");
            return;
        }
        match self.licence_class() {
            LicenceClass::NonCommercial => report.warning(
                "plugin.licence_noncommercial",
                "licence",
                format!("`{}` is non-commercial or restricted", self.licence),
            ),
            LicenceClass::Unknown => {
                report.warning(
                    "plugin.licence_unknown",
                    "licence",
                    format!("`{}` is not a known licence", self.licence),
                );
            }
            LicenceClass::Allowed | LicenceClass::Conditions => {}
        }
    }

    fn check_worlds(&self, report: &mut Report) {
        if self.worlds.is_empty() {
            report.error("plugin.worlds_empty", "worlds", "name at least the base world `plugin`");
        }
        for (i, world) in self.worlds.iter().enumerate() {
            if !known_worlds().any(|known| known == world) {
                report.error(
                    "plugin.world_unknown",
                    format!("worlds/{i}"),
                    format!("this host has no world `{world}`"),
                );
            }
        }
    }

    fn check_requests(&self, report: &mut Report) {
        let requests = &self.requests;
        for (i, scope) in requests.scopes.iter().enumerate() {
            let path = format!("requests/scopes/{i}");
            match scope {
                Scope::Native => report.error("plugin.native_refused", path, "`native` is never granted to a plugin"),
                Scope::Fs => report.error(
                    "plugin.scope_refused",
                    path,
                    "`fs` is never granted to a plugin: ask for `data = true`, its own folder",
                ),
                scope if NEVER_GRANTED.contains(scope) => {
                    report.error("plugin.scope_refused", path, format!("`{scope}` is never granted to a plugin"));
                }
                _ => {}
            }
        }
        let wants_net = requests.scopes.contains(&Scope::Net);
        if wants_net && requests.net_hosts.is_empty() {
            report.error("plugin.net_without_hosts", "requests/net_hosts", "`net` needs at least one host");
        }
        if !wants_net && !requests.net_hosts.is_empty() {
            report.error("plugin.hosts_without_net", "requests/scopes", "`net_hosts` needs the `net` scope");
        }
        for (i, host) in requests.net_hosts.iter().enumerate() {
            if let Some(problem) = net_host_problem(host) {
                report.error("plugin.net_host", format!("requests/net_hosts/{i}"), format!("`{host}` {problem}"));
            }
        }
        if !(1..=MAX_DATA_QUOTA_MB).contains(&requests.data_quota_mb) {
            report.error(
                "plugin.data_quota",
                "requests/data_quota_mb",
                format!("the quota is 1 to {MAX_DATA_QUOTA_MB} MiB"),
            );
        }
    }

    fn check_settings(&self, report: &mut Report) {
        if self.settings.len() > MAX_SETTINGS {
            report.error("plugin.settings_too_many", "settings", format!("at most {MAX_SETTINGS} settings"));
        }
        for (i, setting) in self.settings.iter().enumerate() {
            let path = format!("settings/{i}");
            if !is_setting_key(&setting.key) {
                report.error(
                    "plugin.setting_key",
                    format!("{path}/key"),
                    format!(
                        "`{}` is not a setting key (a lower-case slug of at most {MAX_SETTING_KEY_LEN})",
                        setting.key
                    ),
                );
            }
            if self.settings[..i].iter().any(|earlier| earlier.key == setting.key) {
                report.error("plugin.setting_duplicate", format!("{path}/key"), format!("`{}` twice", setting.key));
            }
            match (setting.kind, &setting.schema) {
                (Some(kind), None) if !kind.admits(&setting.default) => report.error(
                    "plugin.setting_default",
                    format!("{path}/default"),
                    format!("the default is not a {kind:?}"),
                ),
                (Some(_), Some(_)) | (None, None) => {
                    report.error("plugin.setting_type", path, "give exactly one of `type` and `schema`");
                }
                (None, Some(schema)) => {
                    if let Some(problem) = crate::setting_schema::schema_problem(schema) {
                        report.error("plugin.setting_schema", format!("{path}/schema"), problem);
                    } else if let Some(problem) = crate::setting_schema::value_problem(schema, &setting.default) {
                        report.error("plugin.setting_default", format!("{path}/default"), problem);
                    }
                }
                _ => {}
            }
        }
    }
}

/// The key inside a serde message that starts with `prefix` and quotes it in backticks.
fn quoted_key(message: &str, prefix: &str) -> Option<String> {
    let (key, _) = message.strip_prefix(prefix)?.split_once('`')?;
    Some(key.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn component_paths_follow_the_normalised_rule() {
        for ok in ["plugin.wasm", "a/b.wasm", "dir.v2/x.wasm"] {
            assert_eq!(component_path_problem(ok), None, "{ok}");
        }
        for bad in ["", "../x.wasm", "/x.wasm", "./a/../x.wasm", "a//x.wasm", "C:/x.wasm", "a\\x.wasm", "a/."] {
            assert!(component_path_problem(bad).is_some(), "{bad}");
        }
    }

    #[test]
    fn hosts_are_exact_names() {
        for ok in ["example.com", "api.example.com:8443", "localhost:8080", "a-b.example"] {
            assert_eq!(net_host_problem(ok), None, "{ok}");
        }
        for bad in [
            "https://example.com",
            "*.example.com",
            "example.com/x",
            "10.0.0.1",
            "10.0.0.1:80",
            "[::1]:80",
            "::1",
            "localhost",
            "Example.com",
            "example.com:0",
            "example.com:99999",
            "-a.com",
        ] {
            assert!(net_host_problem(bad).is_some(), "{bad}");
        }
    }

    #[test]
    fn spdx_expressions_take_the_best_or_and_the_worst_and() {
        assert_eq!(licence_class("MIT OR Apache-2.0"), LicenceClass::Allowed);
        assert_eq!(licence_class("MIT OR CC-BY-NC-4.0"), LicenceClass::Allowed);
        assert_eq!(licence_class("MIT AND CC-BY-NC-4.0"), LicenceClass::NonCommercial);
        assert_eq!(licence_class("(MIT AND LGPL-3.0-only) OR LicenseRef-Mine"), LicenceClass::Conditions);
        assert_eq!(licence_class("Apache-2.0 WITH LLVM-exception"), LicenceClass::Allowed);
        assert_eq!(licence_class("GPL-2.0-only WITH Classpath-exception-2.0"), LicenceClass::Conditions);
        assert_eq!(licence_class("MIT WITH Commons-Clause"), LicenceClass::NonCommercial, "a restriction, not a grant");
        assert_eq!(licence_class("MIT WITH Made-Up-exception"), LicenceClass::Unknown);
        assert_eq!(licence_class("MIT WITH Commons-Clause OR Apache-2.0"), LicenceClass::Allowed);
        assert_eq!(licence_class("GPL-2.0+"), LicenceClass::Unknown, "a bare + of an unknown id stays unknown");
        assert_eq!(licence_class("LGPL-2.1-or-later+"), LicenceClass::Conditions);
        for malformed in ["", "MIT OR", "(MIT", "MIT)", "AND MIT", "MIT WITH", "MIT WITH (x)"] {
            assert_eq!(licence_class(malformed), LicenceClass::Unknown, "{malformed:?}");
        }
    }
}
