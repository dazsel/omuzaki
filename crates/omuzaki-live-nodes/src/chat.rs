//! The chat command trigger (design §6, D8, D9, category Chat): `chat.command` hears `chat.message`, matches
//! `<prefix><name>` or an alias at the start of the message through [`fold`], and fires with the sender and the
//! parsed arguments, or `denied` below its role, or `cooling` inside its cooldowns.

use omuzaki_live_api::event::payloads::{ChatMessage, Role};
use omuzaki_live_api::ids::{EventKind, Platform};
use omuzaki_live_api::limits::MAX_LIST_ITEMS;
use omuzaki_live_api::node::{
    Category, Input, Node, NodeClass, NodeCx, NodeError, NodeKind, NodeMeta, ParamError, ParamErrors, Ports,
};
use omuzaki_live_api::register_node;
use omuzaki_live_api::value::{PortType, Val};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::flow::{Cooldowns, int};
use crate::fold::fold;
use crate::l10n;

/// The most aliases a command has.
const MAX_ALIASES: usize = 16;

/// The kind `chat.command` listens to.
const CHAT_MESSAGE: &str = "chat.message";

/// The least role a command takes.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MinRole {
    /// Anyone in chat.
    #[default]
    Everyone,
    /// Subscribers (members) and up.
    Subscriber,
    /// VIPs and up.
    Vip,
    /// Moderators and the broadcaster.
    Moderator,
    /// The broadcaster only.
    Broadcaster,
}

impl MinRole {
    /// Its rank on [`Role::rank`]'s scale: everyone is 0.
    const fn rank(self) -> u8 {
        match self {
            MinRole::Everyone => 0,
            MinRole::Subscriber => Role::Subscriber.rank(),
            MinRole::Vip => Role::Vip.rank(),
            MinRole::Moderator => Role::Moderator.rank(),
            MinRole::Broadcaster => Role::Broadcaster.rank(),
        }
    }
}

fn prefix_default() -> String {
    "!".to_owned()
}

const fn yes() -> bool {
    true
}

/// Parameters of `chat.command`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CommandParams {
    /// The command, without its prefix: `hug` for `!hug`.
    #[serde(default)]
    pub name: String,
    /// Other names for the same command.
    #[serde(default)]
    pub aliases: Vec<String>,
    /// What starts a command.
    #[serde(default = "prefix_default")]
    pub prefix: String,
    /// The least role that may use it.
    #[serde(default)]
    pub min_role: MinRole,
    /// Seconds between uses by anyone; 0 for none.
    #[serde(default)]
    pub cooldown_s: f64,
    /// Seconds between uses by the same person; 0 for none.
    #[serde(default)]
    pub user_cooldown_s: f64,
    /// The platforms it answers on; empty for all.
    #[serde(default)]
    pub platforms: Vec<Platform>,
    /// The broadcaster skips the cooldowns.
    #[serde(default = "yes")]
    pub broadcaster_bypass: bool,
}

impl Default for CommandParams {
    fn default() -> Self {
        Self {
            name: String::new(),
            aliases: Vec::new(),
            prefix: prefix_default(),
            min_role: MinRole::Everyone,
            cooldown_s: 0.0,
            user_cooldown_s: 0.0,
            platforms: Vec::new(),
            broadcaster_bypass: true,
        }
    }
}

/// Whether `word` can be a command's name or prefix: not empty, no spaces.
fn one_word(word: &str) -> bool {
    !word.is_empty() && !word.chars().any(char::is_whitespace)
}

/// The words of `rest`: split on whitespace, a quoted stretch kept whole (without its quotes), `@name` as the login
/// `name`.
fn split_args(rest: &str) -> Vec<String> {
    let mut args = Vec::new();
    let mut chars = rest.chars().peekable();
    while args.len() < MAX_LIST_ITEMS {
        while chars.next_if(|c| c.is_whitespace()).is_some() {}
        let Some(&first) = chars.peek() else { break };
        let mut word = String::new();
        if let Some(close) = closing_quote(first) {
            chars.next();
            for c in chars.by_ref() {
                if c == close {
                    break;
                }
                word.push(c);
            }
            args.push(word);
            continue;
        }
        while let Some(c) = chars.next_if(|c| !c.is_whitespace()) {
            word.push(c);
        }
        args.push(match word.strip_prefix('@') {
            Some(login) if !login.is_empty() => login.to_lowercase(),
            _ => word,
        });
    }
    args
}

/// The quote that closes `open`, if it opens one.
fn closing_quote(open: char) -> Option<char> {
    match open {
        '"' => Some('"'),
        '“' => Some('”'),
        '「' => Some('」'),
        _ => None,
    }
}

/// Fires on a chat command: the sender and the arguments, or `denied`, or `cooling`.
#[derive(Debug)]
pub struct Command {
    /// The folded `<prefix><name>` and `<prefix><alias>`.
    calls: Vec<String>,
    min_role: MinRole,
    cooldown_s: f64,
    user_cooldown_s: f64,
    platforms: Vec<Platform>,
    broadcaster_bypass: bool,
    global: Option<f64>,
    users: Cooldowns,
}

impl NodeKind for Command {
    type Params = CommandParams;

    fn meta() -> NodeMeta {
        NodeMeta::new("chat.command", "When someone types a chat command.", NodeClass::Trigger, Category::Chat)
    }

    fn ports(_: &CommandParams) -> Ports {
        Ports::new()
            .value_out("user", PortType::Text, "Who typed it: their display name.")
            .value_out("user_id", PortType::Text, "Their platform's id for them.")
            .value_out("login", PortType::Text, "Their handle.")
            .value_out("roles", PortType::List, "Their roles: broadcaster, moderator, vip, subscriber.")
            .value_out("platform", PortType::Text, "The platform it came from.")
            .value_out("args", PortType::List, "The words after the command; \"quoted words\" stay together.")
            .value_out("rest", PortType::Text, "Everything after the command.")
            .value_out("left", PortType::Number, "Seconds left in the cooldown.")
            .event_out("fired", "Someone allowed typed the command.")
            .event_out("denied", "Someone below the command's role typed it.")
            .event_out("cooling", "It was typed inside a cooldown; the time left is on `left`.")
    }

    fn listens(_: &CommandParams) -> Vec<EventKind> {
        vec![EventKind::from_static(CHAT_MESSAGE)]
    }

    fn check(params: &CommandParams) -> Result<(), ParamErrors> {
        let mut errors = Vec::new();
        if !one_word(&params.name) {
            errors.push(ParamError::new("name", l10n::command_not_one_word()));
        }
        if !one_word(&params.prefix) {
            errors.push(ParamError::new("prefix", l10n::command_not_one_word()));
        }
        if params.aliases.len() > MAX_ALIASES {
            errors.push(ParamError::new(
                "aliases",
                l10n::count_out_of_range(int(params.aliases.len()), 0, int(MAX_ALIASES)),
            ));
        }
        for (i, alias) in params.aliases.iter().enumerate() {
            if !one_word(alias) {
                errors.push(ParamError::new(format!("aliases.{i}"), l10n::command_not_one_word()));
            }
        }
        for (key, seconds) in [("cooldown_s", params.cooldown_s), ("user_cooldown_s", params.user_cooldown_s)] {
            if !seconds.is_finite() || seconds < 0.0 {
                errors.push(ParamError::new(key, l10n::bad_seconds()));
            }
        }
        if errors.is_empty() { Ok(()) } else { Err(ParamErrors(errors)) }
    }

    fn new(params: CommandParams) -> Self {
        let calls = std::iter::once(&params.name)
            .chain(&params.aliases)
            .map(|name| fold(&format!("{}{name}", params.prefix)))
            .collect();
        Self {
            calls,
            min_role: params.min_role,
            cooldown_s: params.cooldown_s,
            user_cooldown_s: params.user_cooldown_s,
            platforms: params.platforms,
            broadcaster_bypass: params.broadcaster_bypass,
            global: None,
            users: Cooldowns::default(),
        }
    }
}

impl Command {
    /// The text after the command, if `text` starts with one of its calls followed by whitespace or the end.
    fn matches<'t>(&self, text: &'t str) -> Option<&'t str> {
        let text = text.trim_start();
        let end = text.find(char::is_whitespace).unwrap_or(text.len());
        let (head, rest) = text.split_at(end);
        let head = fold(head);
        self.calls.contains(&head).then(|| rest.trim_start())
    }

    fn on_message(&mut self, message: &ChatMessage, cx: &mut NodeCx<'_>) -> Result<(), NodeError> {
        if !self.platforms.is_empty() && !self.platforms.contains(&message.platform) {
            return Ok(());
        }
        let Some(rest) = self.matches(&message.text) else { return Ok(()) };
        let user = &message.user;
        let roles: Vec<Val> = user
            .roles
            .iter()
            .filter_map(|role| serde_json::to_value(role).ok())
            .filter_map(|role| role.as_str().map(Val::text))
            .collect();
        cx.set("user", Val::text(user.name.as_str()))?;
        cx.set("user_id", Val::text(user.id.as_str()))?;
        cx.set("login", Val::text(user.login.as_str()))?;
        cx.set("roles", Val::List(roles.into()))?;
        cx.set("platform", Val::text(message.platform.as_str()))?;
        cx.set("args", Val::List(split_args(rest).into_iter().map(Val::text).collect::<Vec<_>>().into()))?;
        cx.set("rest", Val::text(rest))?;
        if user.rank() < self.min_role.rank() {
            return cx.fire("denied");
        }
        let now = cx.now();
        let bypass = self.broadcaster_bypass && user.roles.contains(&Role::Broadcaster);
        if !bypass {
            let global = self.global.map_or(0.0, |last| last + self.cooldown_s - now.0);
            let mine = self.users.left(&user.id, self.user_cooldown_s, now).unwrap_or(0.0);
            let left = global.max(mine);
            if left > 0.0 {
                cx.set("left", Val::Number(left))?;
                return cx.fire("cooling");
            }
            self.global = Some(now.0);
            self.users.pass(&user.id, now);
        }
        cx.fire("fired")
    }
}

impl Node for Command {
    fn handle(&mut self, input: Input<'_>, cx: &mut NodeCx<'_>) -> Result<(), NodeError> {
        match input {
            Input::Trigger { event, .. } => match event.decode::<ChatMessage>() {
                Some(Ok(message)) => self.on_message(&message, cx),
                // Another kind, or a message the platform layer should have refused: not a command.
                Some(Err(_)) | None => Ok(()),
            },
            Input::Event { port, .. } => Err(NodeError::unknown_port(port)),
            Input::Start | Input::Stop | Input::Wake { .. } | Input::Reply { .. } => Ok(()),
            other => Err(NodeError::unhandled(&other)),
        }
    }
}
register_node!(Command);
