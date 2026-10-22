//! The normalised kinds (design D9): what every platform adapter publishes, so a chat command, a redeem or a raid works
//! on every platform at once, and the engine's own kinds. BR-10 adds fields (optional ones only, design D20) and
//! never renames them; `hotkey` and `voice.command` are registered by their own crates.
//!
//! Every type is `#[non_exhaustive]` and built with `new` (the fields design D9 fixes) and `with_*` (the optional
//! ones). Every field is documented: its doc comment becomes the `event-<kind>--<field>` text.

use std::collections::BTreeSet;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::EventPayload;
use crate::ids::{GraphId, NodeId, Platform, ShowId, VarName};
use crate::register_event_kind;
use crate::show::VarScope;

/// Implements [`EventPayload`] with the kind and the summary (the first line of the type's doc comment; a test keeps
/// them equal).
macro_rules! payload {
    ($ty:ident, $kind:literal, $summary:literal) => {
        impl EventPayload for $ty {
            const KIND: &'static str = $kind;
            const SUMMARY: &'static str = $summary;
        }
    };
}

// ── Who and where ───────────────────────────────────────────────────────────────────────────────────────────────

/// A channel on a platform: whose chat or stream an event belongs to.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct ChannelRef {
    /// The platform's stable id for the channel.
    pub id: String,
    /// The channel's name, as shown.
    pub name: String,
}

impl ChannelRef {
    /// The channel `id`, shown as `name`.
    pub fn new(id: impl Into<String>, name: impl Into<String>) -> Self {
        Self { id: id.into(), name: name.into() }
    }
}

/// A role a user holds in a channel, normalised across platforms (`YouTube`'s owner is a broadcaster and its members
/// subscribers; Bilibili's room admins are moderators and its guards subscribers).
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Role {
    /// The channel's owner.
    Broadcaster,
    /// A moderator.
    Moderator,
    /// A VIP.
    Vip,
    /// A subscriber or member.
    Subscriber,
}

impl Role {
    /// Its rank for "at least" checks: broadcaster 4, moderator 3, VIP 2, subscriber 1. Nobody (no role) is 0.
    pub const fn rank(self) -> u8 {
        match self {
            Role::Broadcaster => 4,
            Role::Moderator => 3,
            Role::Vip => 2,
            Role::Subscriber => 1,
        }
    }
}

/// A user on a platform.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct ChatUser {
    /// The platform's stable user id: the key for cooldowns, voices and memory.
    pub id: String,
    /// The handle.
    pub login: String,
    /// The display name.
    pub name: String,
    /// The user's roles in this channel.
    #[serde(default)]
    pub roles: BTreeSet<Role>,
    /// The platform's raw badge ids, for platform-specific conditions.
    #[serde(default)]
    pub badges: Vec<String>,
    /// The user's colour as `#rrggbb`, when the platform has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub colour: Option<String>,
}

impl ChatUser {
    /// The user `id`, with handle `login` and display name `name`, with no roles, badges or colour.
    pub fn new(id: impl Into<String>, login: impl Into<String>, name: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            login: login.into(),
            name: name.into(),
            roles: BTreeSet::new(),
            badges: Vec::new(),
            colour: None,
        }
    }

    /// With these roles.
    #[must_use]
    pub fn with_roles(mut self, roles: impl IntoIterator<Item = Role>) -> Self {
        self.roles = roles.into_iter().collect();
        self
    }

    /// With these badge ids.
    #[must_use]
    pub fn with_badges(mut self, badges: Vec<String>) -> Self {
        self.badges = badges;
        self
    }

    /// With this colour (`#rrggbb`).
    #[must_use]
    pub fn with_colour(mut self, colour: impl Into<String>) -> Self {
        self.colour = Some(colour.into());
        self
    }

    /// The rank of the user's highest role; 0 with none.
    pub fn rank(&self) -> u8 {
        self.roles.iter().map(|role| role.rank()).max().unwrap_or(0)
    }
}

/// Where an emote is in a message's text.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct EmoteSpan {
    /// The byte offset in the text where it starts.
    pub start: u32,
    /// The byte offset in the text where it ends (exclusive).
    pub end: u32,
    /// The platform's id for the emote.
    pub id: String,
    /// The emote's name.
    pub name: String,
}

impl EmoteSpan {
    /// The emote `id` called `name`, at bytes `start..end` of the text.
    pub fn new(start: u32, end: u32, id: impl Into<String>, name: impl Into<String>) -> Self {
        Self { start, end, id: id.into(), name: name.into() }
    }
}

/// An amount of money.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct Money {
    /// The amount in millionths of the currency's unit.
    pub micros: i64,
    /// The ISO 4217 currency code (`USD`, `JPY`).
    pub currency: String,
}

impl Money {
    /// `micros` millionths of `currency`.
    pub fn new(micros: i64, currency: impl Into<String>) -> Self {
        Self { micros, currency: currency.into() }
    }
}

/// A channel-points reward.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct Reward {
    /// The platform's id for the reward.
    pub id: String,
    /// The reward's title, as the streamer set it.
    pub title: String,
    /// What it costs, in channel points.
    pub cost: u32,
}

impl Reward {
    /// The reward `id` titled `title`, costing `cost`.
    pub fn new(id: impl Into<String>, title: impl Into<String>, cost: u32) -> Self {
        Self { id: id.into(), title: title.into(), cost }
    }
}

/// The phase of a poll, prediction or hype train.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Phase {
    /// It began.
    Begin,
    /// It moved on.
    Progress,
    /// It closed to new entries.
    Lock,
    /// It ended.
    End,
}

// ── Chat and engagement ────────────────────────────────────────────────────────────────────────────────────────

/// A chat message.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct ChatMessage {
    /// The platform it was sent on.
    pub platform: Platform,
    /// Whose chat it was sent in.
    pub channel: ChannelRef,
    /// The platform's id for the message, for replies, deletes and moderation.
    pub message_id: String,
    /// Who sent it.
    pub user: ChatUser,
    /// The text as the platform sent it, with emotes as their names.
    pub text: String,
    /// Where the emotes are in the text.
    #[serde(default)]
    pub emotes: Vec<EmoteSpan>,
    /// The id of the message it replies to.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reply_to: Option<String>,
    /// Whether it is an action (`/me`).
    #[serde(default)]
    pub action: bool,
    /// Whether it is the user's first message in this channel, when the platform says so.
    #[serde(default)]
    pub first_message: bool,
}
payload!(ChatMessage, "chat.message", "A chat message.");
register_event_kind!(ChatMessage, droppable);

impl ChatMessage {
    /// The message `message_id` from `user` in `channel` on `platform`, saying `text`.
    pub fn new(
        platform: Platform,
        channel: ChannelRef,
        message_id: impl Into<String>,
        user: ChatUser,
        text: impl Into<String>,
    ) -> Self {
        Self {
            platform,
            channel,
            message_id: message_id.into(),
            user,
            text: text.into(),
            emotes: Vec::new(),
            reply_to: None,
            action: false,
            first_message: false,
        }
    }

    /// With these emotes.
    #[must_use]
    pub fn with_emotes(mut self, emotes: Vec<EmoteSpan>) -> Self {
        self.emotes = emotes;
        self
    }

    /// Replying to the message `reply_to`.
    #[must_use]
    pub fn with_reply_to(mut self, reply_to: impl Into<String>) -> Self {
        self.reply_to = Some(reply_to.into());
        self
    }

    /// An action (`/me`) or not.
    #[must_use]
    pub fn with_action(mut self, action: bool) -> Self {
        self.action = action;
        self
    }

    /// The user's first message here, or not.
    #[must_use]
    pub fn with_first_message(mut self, first_message: bool) -> Self {
        self.first_message = first_message;
        self
    }
}

/// Someone followed the channel.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct Follow {
    /// The platform.
    pub platform: Platform,
    /// The channel followed.
    pub channel: ChannelRef,
    /// Who followed.
    pub user: ChatUser,
}
payload!(Follow, "follow", "Someone followed the channel.");
register_event_kind!(Follow);

impl Follow {
    /// `user` followed `channel` on `platform`.
    pub fn new(platform: Platform, channel: ChannelRef, user: ChatUser) -> Self {
        Self { platform, channel, user }
    }
}

/// Someone subscribed, or resubscribed, or was gifted a subscription.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct Sub {
    /// The platform.
    pub platform: Platform,
    /// The channel subscribed to.
    pub channel: ChannelRef,
    /// Who holds the subscription.
    pub user: ChatUser,
    /// The tier: `1000`, `2000`, `3000`, `prime`, or the platform's level name.
    pub tier: String,
    /// How many months in total.
    pub months: u32,
    /// What the subscriber wrote, if anything.
    #[serde(default)]
    pub message: String,
    /// Whether the subscription was a gift.
    #[serde(default)]
    pub gift: bool,
    /// Who gave it, for a gift that wasn't anonymous.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gifter: Option<ChatUser>,
}
payload!(Sub, "sub", "Someone subscribed, or resubscribed, or was gifted a subscription.");
register_event_kind!(Sub);

impl Sub {
    /// `user` holds a `tier` subscription to `channel`, `months` months in total.
    pub fn new(platform: Platform, channel: ChannelRef, user: ChatUser, tier: impl Into<String>, months: u32) -> Self {
        Self { platform, channel, user, tier: tier.into(), months, message: String::new(), gift: false, gifter: None }
    }

    /// With the subscriber's message.
    #[must_use]
    pub fn with_message(mut self, message: impl Into<String>) -> Self {
        self.message = message.into();
        self
    }

    /// A gift, from `gifter` (`None`: anonymous).
    #[must_use]
    pub fn with_gift(mut self, gifter: Option<ChatUser>) -> Self {
        self.gift = true;
        self.gifter = gifter;
        self
    }
}

/// Someone gifted subscriptions to the community.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct SubGift {
    /// The platform.
    pub platform: Platform,
    /// The channel.
    pub channel: ChannelRef,
    /// Who gifted them; absent when anonymous.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gifter: Option<ChatUser>,
    /// How many subscriptions.
    pub count: u32,
    /// Their tier.
    pub tier: String,
}
payload!(SubGift, "sub_gift", "Someone gifted subscriptions to the community.");
register_event_kind!(SubGift);

impl SubGift {
    /// `count` anonymous `tier` subscriptions gifted in `channel`.
    pub fn new(platform: Platform, channel: ChannelRef, count: u32, tier: impl Into<String>) -> Self {
        Self { platform, channel, gifter: None, count, tier: tier.into() }
    }

    /// Gifted by `gifter`.
    #[must_use]
    pub fn with_gifter(mut self, gifter: ChatUser) -> Self {
        self.gifter = Some(gifter);
        self
    }
}

/// Someone cheered with bits.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct Cheer {
    /// The platform.
    pub platform: Platform,
    /// The channel.
    pub channel: ChannelRef,
    /// Who cheered; absent when anonymous.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user: Option<ChatUser>,
    /// How many bits.
    pub bits: u32,
    /// The message that came with them.
    #[serde(default)]
    pub message: String,
}
payload!(Cheer, "cheer", "Someone cheered with bits.");
register_event_kind!(Cheer);

impl Cheer {
    /// An anonymous cheer of `bits` in `channel`, saying `message`.
    pub fn new(platform: Platform, channel: ChannelRef, bits: u32, message: impl Into<String>) -> Self {
        Self { platform, channel, user: None, bits, message: message.into() }
    }

    /// From `user`.
    #[must_use]
    pub fn with_user(mut self, user: ChatUser) -> Self {
        self.user = Some(user);
        self
    }
}

/// Someone tipped: a Super Chat or Super Sticker, or a tip service through a plugin.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct Tip {
    /// The platform or tip service.
    pub platform: Platform,
    /// The channel.
    pub channel: ChannelRef,
    /// Who tipped.
    pub user: ChatUser,
    /// How much.
    pub amount: Money,
    /// The message that came with it.
    #[serde(default)]
    pub message: String,
}
payload!(Tip, "tip", "Someone tipped: a Super Chat or Super Sticker, or a tip service through a plugin.");
register_event_kind!(Tip);

impl Tip {
    /// `user` tipped `amount` in `channel`.
    pub fn new(platform: Platform, channel: ChannelRef, user: ChatUser, amount: Money) -> Self {
        Self { platform, channel, user, amount, message: String::new() }
    }

    /// With the tipper's message.
    #[must_use]
    pub fn with_message(mut self, message: impl Into<String>) -> Self {
        self.message = message.into();
        self
    }
}

/// Someone redeemed a channel-points reward.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct Redeem {
    /// The platform.
    pub platform: Platform,
    /// The channel.
    pub channel: ChannelRef,
    /// Who redeemed it.
    pub user: ChatUser,
    /// The reward.
    pub reward: Reward,
    /// What the user typed, for a reward that asks for text.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<String>,
    /// The platform's id for this redemption, to fulfil or refund it.
    pub redemption_id: String,
}
payload!(Redeem, "redeem", "Someone redeemed a channel-points reward.");
register_event_kind!(Redeem);

impl Redeem {
    /// `user` redeemed `reward` in `channel`; the platform calls it `redemption_id`.
    pub fn new(
        platform: Platform,
        channel: ChannelRef,
        user: ChatUser,
        reward: Reward,
        redemption_id: impl Into<String>,
    ) -> Self {
        Self { platform, channel, user, reward, input: None, redemption_id: redemption_id.into() }
    }

    /// With the text the user typed.
    #[must_use]
    pub fn with_input(mut self, input: impl Into<String>) -> Self {
        self.input = Some(input.into());
        self
    }
}

/// Another channel raided this one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct Raid {
    /// The platform.
    pub platform: Platform,
    /// The channel raided.
    pub channel: ChannelRef,
    /// The raiding channel.
    pub from: ChatUser,
    /// How many viewers came along.
    pub viewers: u32,
}
payload!(Raid, "raid", "Another channel raided this one.");
register_event_kind!(Raid);

impl Raid {
    /// `from` raided `channel` with `viewers` viewers.
    pub fn new(platform: Platform, channel: ChannelRef, from: ChatUser, viewers: u32) -> Self {
        Self { platform, channel, from, viewers }
    }
}

/// Someone used a power-up.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct PowerUp {
    /// The platform.
    pub platform: Platform,
    /// The channel.
    pub channel: ChannelRef,
    /// Who used it.
    pub user: ChatUser,
    /// Which power-up, as the platform names it.
    pub kind: String,
    /// What it cost, in bits.
    pub bits: u32,
}
payload!(PowerUp, "power_up", "Someone used a power-up.");
register_event_kind!(PowerUp);

impl PowerUp {
    /// `user` used the power-up `kind` for `bits` bits.
    pub fn new(platform: Platform, channel: ChannelRef, user: ChatUser, kind: impl Into<String>, bits: u32) -> Self {
        Self { platform, channel, user, kind: kind.into(), bits }
    }
}

/// A poll began, moved on, locked or ended.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct Poll {
    /// The platform.
    pub platform: Platform,
    /// The channel.
    pub channel: ChannelRef,
    /// Where the poll is.
    pub phase: Phase,
}
payload!(Poll, "poll", "A poll began, moved on, locked or ended.");
register_event_kind!(Poll);

impl Poll {
    /// A poll in `channel` at `phase`.
    pub fn new(platform: Platform, channel: ChannelRef, phase: Phase) -> Self {
        Self { platform, channel, phase }
    }
}

/// A prediction began, moved on, locked or ended.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct Prediction {
    /// The platform.
    pub platform: Platform,
    /// The channel.
    pub channel: ChannelRef,
    /// Where the prediction is.
    pub phase: Phase,
}
payload!(Prediction, "prediction", "A prediction began, moved on, locked or ended.");
register_event_kind!(Prediction);

impl Prediction {
    /// A prediction in `channel` at `phase`.
    pub fn new(platform: Platform, channel: ChannelRef, phase: Phase) -> Self {
        Self { platform, channel, phase }
    }
}

/// A hype train began, moved on or ended.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct HypeTrain {
    /// The platform.
    pub platform: Platform,
    /// The channel.
    pub channel: ChannelRef,
    /// Where the hype train is.
    pub phase: Phase,
}
payload!(HypeTrain, "hype_train", "A hype train began, moved on or ended.");
register_event_kind!(HypeTrain);

impl HypeTrain {
    /// A hype train in `channel` at `phase`.
    pub fn new(platform: Platform, channel: ChannelRef, phase: Phase) -> Self {
        Self { platform, channel, phase }
    }
}

// ── The engine's own kinds ─────────────────────────────────────────────────────────────────────────────────────

/// The show started or is stopping (`show.started`, `show.stopping`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct ShowLifecycle {
    /// The show.
    pub show: ShowId,
}

impl EventPayload for ShowLifecycle {
    const KIND: &'static str = Self::STARTED;
    const SUMMARY: &'static str = "The show started or is stopping (`show.started`, `show.stopping`).";

    fn is_kind(kind: &str) -> bool {
        kind == Self::STARTED || kind == Self::STOPPING
    }
}
register_event_kind!(ShowLifecycle);
register_event_kind!(ShowLifecycle, kind = "show.stopping");

impl ShowLifecycle {
    /// The kind published when a show starts.
    pub const STARTED: &'static str = "show.started";
    /// The kind published when a show is stopping.
    pub const STOPPING: &'static str = "show.stopping";

    /// About `show`.
    pub fn new(show: ShowId) -> Self {
        Self { show }
    }
}

/// A variable changed during the last step: the first old value and the last new one.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct VarChanged {
    /// The variable.
    pub name: VarName,
    /// Its scope.
    pub scope: VarScope,
    /// Its value before the step.
    pub old: Value,
    /// Its value after the step.
    pub new: Value,
}
payload!(
    VarChanged,
    "var.changed",
    "A variable changed during the last step: the first old value and the last new one."
);
register_event_kind!(VarChanged);

impl VarChanged {
    /// `name` (of `scope`) went from `old` to `new`.
    pub fn new(name: VarName, scope: VarScope, old: Value, new: Value) -> Self {
        Self { name, scope, old, new }
    }
}

/// The synthetic root of a token a node started outside any event: from its start, or a timer's own wake.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[non_exhaustive]
pub struct EngineTick {
    /// The graph of the node.
    pub graph: GraphId,
    /// The node that started the token.
    pub node: NodeId,
}
payload!(
    EngineTick,
    "engine.tick",
    "The synthetic root of a token a node started outside any event: from its start, or a timer's own wake."
);
register_event_kind!(EngineTick);

impl EngineTick {
    /// Started by `graph`'s node `node`.
    pub fn new(graph: GraphId, node: NodeId) -> Self {
        Self { graph, node }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_users_rank_is_their_highest_role() {
        let user = |roles: &[Role]| ChatUser::new("1", "kiri", "Kiri").with_roles(roles.iter().copied());
        assert_eq!(user(&[]).rank(), 0);
        assert_eq!(user(&[Role::Moderator, Role::Subscriber]).rank(), Role::Moderator.rank());
        let ranks: Vec<u8> = [Role::Broadcaster, Role::Moderator, Role::Vip, Role::Subscriber].map(Role::rank).to_vec();
        assert!(ranks.windows(2).all(|w| w[0] > w[1]) && ranks[3] > 0);
    }
}
