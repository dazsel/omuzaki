//! The permission summary a person reads before turning a plugin on, and the difference an update asks for (design
//! `plugins.md` §3.2, §8, §13; PL-04 "a permission summary before install").
//!
//! A pure function of the manifest, so the plugin manager panel and the store show the same card. Every sentence is a
//! message id with typed arguments ([`Text`]), never a hard-coded string.

use omuzaki_l10n_api::Text;
use omuzaki_ops_api::Scope;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::{LicenceClass, Manifest};

omuzaki_l10n_api::messages! {
    /// A plugin wants to read the project.
    pub perm_read = "plugin-api-perm-read" () => "Read the project";
    /// A plugin wants to edit characters.
    pub perm_edit_character = "plugin-api-perm-edit-character" () => "Change characters";
    /// A plugin wants to edit scenes.
    pub perm_edit_scene = "plugin-api-perm-edit-scene" () => "Change scenes, props, lights and cameras";
    /// A plugin wants to edit the library.
    pub perm_edit_library = "plugin-api-perm-edit-library" () => "Change the library";
    /// A plugin wants to change settings.
    pub perm_edit_settings = "plugin-api-perm-edit-settings" () => "Change your settings";
    /// A plugin wants to render.
    pub perm_render = "plugin-api-perm-render" () => "Render previews and images";
    /// A plugin wants to drive the avatar live.
    pub perm_perform = "plugin-api-perm-perform" () => "Move the avatar";
    /// A plugin wants to go live; asked on every use.
    pub perm_broadcast = "plugin-api-perm-broadcast" () => "Go live and switch scenes, asking you each time";
    /// A plugin wants to publish; asked on every use.
    pub perm_publish = "plugin-api-perm-publish" () => "Publish things outside this computer, asking you each time";
    /// A plugin wants to spend money; asked on every use.
    pub perm_spend = "plugin-api-perm-spend" () => "Spend money on paid services, asking you each time";
    /// A plugin wants to reach the network; `hosts` lists every host it may reach.
    pub perm_net = "plugin-api-perm-net" (hosts: List) => "Reach { $hosts }";
    /// A scope a plugin is never granted (refused at install, shown if a card is ever built for it).
    pub perm_refused = "plugin-api-perm-refused" (scope: Id) => "{ $scope } (never granted to a plugin)";
    /// A plugin wants its own data folder; `quota` is in mebibytes (MiB).
    pub perm_data = "plugin-api-perm-data" (quota: Int) => "Keep its own files, up to { $quota } MiB";
}

/// How much a line can hurt; the card colours it.
#[derive(Copy, Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Risk {
    /// Looks only.
    Low,
    /// Changes the project or drives the avatar.
    Medium,
    /// Reaches outside the project or the machine.
    High,
}

/// The risk of a scope: `read` low; `edit:*`, `perform` and `render` medium; everything that reaches outside the
/// project high, as is any scope this table does not name.
pub fn risk(scope: Scope) -> Risk {
    match scope {
        Scope::Read => Risk::Low,
        Scope::EditCharacter
        | Scope::EditScene
        | Scope::EditLibrary
        | Scope::EditSettings
        | Scope::Render
        | Scope::Perform => Risk::Medium,
        _ => Risk::High,
    }
}

/// The scopes a plugin session is asked about on every use, never for a session or a project (design §3.2).
pub const PER_USE: [Scope; 3] = [Scope::Publish, Scope::Broadcast, Scope::Spend];

/// One line of the card.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PermLine {
    /// What it grants.
    pub scope: Scope,
    /// How much it can hurt.
    pub risk: Risk,
    /// The sentence the person reads.
    pub text: Text,
    /// The hosts, for `net`.
    pub detail: Option<String>,
}

/// The line of the card for the plugin's own data folder.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DataLine {
    /// The quota in MiB.
    pub quota_mb: u32,
    /// The sentence the person reads.
    pub text: Text,
}

/// The card for one manifest.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PermissionSummary {
    /// One line per requested scope, highest risk first, then by scope name.
    pub lines: Vec<PermLine>,
    /// Its data folder, when it asks for one.
    pub data: Option<DataLine>,
    /// The requested scopes asked about on every use.
    pub needs_person_each_use: Vec<Scope>,
    /// Its licence's class.
    pub licence: LicenceClass,
    /// Whether it has no publisher.
    pub unsigned: bool,
}

/// What an update asks for beyond what the person approved.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PermissionDiff {
    /// New scopes, as card lines in the card's order.
    pub added: Vec<PermLine>,
    /// Approved scopes the update no longer asks for, by scope name.
    pub removed: Vec<Scope>,
}

impl PermissionDiff {
    /// Whether the update asks for nothing new and drops nothing.
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty()
    }
}

/// The sentence for `scope` in a plugin whose network hosts are `hosts`.
fn text_of(scope: Scope, hosts: &[String]) -> Text {
    match scope {
        Scope::Read => perm_read(),
        Scope::EditCharacter => perm_edit_character(),
        Scope::EditScene => perm_edit_scene(),
        Scope::EditLibrary => perm_edit_library(),
        Scope::EditSettings => perm_edit_settings(),
        Scope::Render => perm_render(),
        Scope::Perform => perm_perform(),
        Scope::Broadcast => perm_broadcast(),
        Scope::Publish => perm_publish(),
        Scope::Spend => perm_spend(),
        Scope::Net => perm_net(hosts.iter().map(|h| omuzaki_l10n_api::Arg::Id(h.clone())).collect()),
        other => perm_refused(other.name()),
    }
}

fn line(scope: Scope, hosts: &[String]) -> PermLine {
    let detail = (scope == Scope::Net).then(|| hosts.join(", "));
    PermLine { scope, risk: risk(scope), text: text_of(scope, hosts), detail }
}

/// The card's lines for `scopes`, each once, highest risk first, then by scope name.
fn lines(scopes: impl IntoIterator<Item = Scope>, hosts: &[String]) -> Vec<PermLine> {
    let mut scopes: Vec<Scope> = scopes.into_iter().collect();
    scopes.sort_by(|a, b| risk(*b).cmp(&risk(*a)).then_with(|| a.name().cmp(b.name())));
    scopes.dedup();
    scopes.into_iter().map(|scope| line(scope, hosts)).collect()
}

/// The card for `manifest`.
pub fn summarise(manifest: &Manifest) -> PermissionSummary {
    let requests = &manifest.requests;
    let lines = lines(requests.scopes.iter().copied(), &requests.net_hosts);
    let needs_person_each_use = lines.iter().map(|l| l.scope).filter(|s| PER_USE.contains(s)).collect();
    let data = requests
        .data
        .then(|| DataLine { quota_mb: requests.data_quota_mb, text: perm_data(i64::from(requests.data_quota_mb)) });
    PermissionSummary {
        lines,
        data,
        needs_person_each_use,
        licence: manifest.licence_class(),
        unsigned: manifest.publisher.is_none(),
    }
}

/// What `new` asks for that the person has not approved (`approved`), and what approved scopes it drops.
pub fn diff(new: &Manifest, approved: &[Scope]) -> PermissionDiff {
    let requests = &new.requests;
    let added = lines(requests.scopes.iter().copied().filter(|s| !approved.contains(s)), &requests.net_hosts);
    let mut removed: Vec<Scope> = approved.iter().copied().filter(|s| !requests.scopes.contains(s)).collect();
    removed.sort_by_key(|s| s.name());
    removed.dedup();
    PermissionDiff { added, removed }
}
