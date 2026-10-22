//! Twitch `EventSub` over WebSocket, as data: the messages the socket sends, the subscriptions the adapter makes, and
//! the normalisation of each notification into an automation event (design `docs/design/automation.md` D9).
//!
//! Written from Twitch's public `EventSub` reference. A socket says `session_welcome` first (with the session id the
//! subscriptions name), then `session_keepalive` whenever it has been quiet for the keepalive time, `notification`
//! for each event, `session_reconnect` before Twitch moves the connection (the subscriptions carry over to the new
//! URL), and `revocation` when a subscription ends (the token lost a scope, the user changed their password).

use omuzaki_core::time::Seconds;
use omuzaki_live_api::event::payloads::{
    ChannelRef, ChatMessage, ChatUser, Cheer, EmoteSpan, Follow, Raid, Redeem, Reward, Role, Sub,
};
use omuzaki_live_api::event::{EventPayload, LiveEvent, Source};
use omuzaki_live_api::ids::Platform;
use serde::Deserialize;
use serde_json::{Value, json};

/// Twitch's `EventSub` WebSocket.
pub const EVENTSUB_URL: &str = "wss://eventsub.wss.twitch.tv/ws";
/// The Helix API.
pub const HELIX_URL: &str = "https://api.twitch.tv/helix";
/// The OAuth service (token validation).
pub const OAUTH_URL: &str = "https://id.twitch.tv/oauth2";

/// The platform id every Twitch event carries.
pub const PLATFORM: &str = "twitch";

/// The platform as an id (always `Some`: `twitch` is a valid id).
#[must_use]
pub fn platform() -> Option<Platform> {
    Platform::new(PLATFORM).ok()
}

/// One message from the socket.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct Message {
    /// What it is.
    pub metadata: Metadata,
    /// Its content, which depends on the type.
    #[serde(default)]
    pub payload: Value,
}

/// A message's metadata.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct Metadata {
    /// Unique per message; a redelivered notification keeps it.
    pub message_id: String,
    /// `session_welcome`, `session_keepalive`, `notification`, `session_reconnect` or `revocation`.
    pub message_type: String,
    /// When Twitch sent it (RFC 3339).
    #[serde(default)]
    pub message_timestamp: String,
    /// For a notification or a revocation: the subscription's type.
    #[serde(default)]
    pub subscription_type: Option<String>,
}

/// A message, by what the adapter does with it.
#[derive(Clone, Debug, PartialEq)]
pub enum Frame {
    /// The session is open: subscribe within ten seconds, naming `session`.
    Welcome {
        /// The session id.
        session: String,
        /// Seconds of silence after which the connection is dead.
        keepalive: u64,
    },
    /// Still alive.
    Keepalive,
    /// An event.
    Notification {
        /// The message id: the event's origin id, so a redelivery is dropped.
        message_id: String,
        /// The subscription type (`channel.follow`).
        kind: String,
        /// The event, as Twitch sent it.
        event: Value,
    },
    /// Connect to `url` now; the old connection closes after the new welcome.
    Reconnect {
        /// Where to.
        url: String,
    },
    /// A subscription ended.
    Revocation {
        /// Its type.
        kind: String,
        /// Why (`authorization_revoked`, `user_removed`, `version_removed`).
        status: String,
    },
    /// A message type this adapter doesn't know.
    Other(String),
}

/// Reads one text message from the socket.
///
/// # Errors
/// The JSON error when the text isn't a message.
pub fn parse(text: &str) -> Result<Frame, serde_json::Error> {
    let message: Message = serde_json::from_str(text)?;
    let payload = &message.payload;
    let session = &payload["session"];
    Ok(match message.metadata.message_type.as_str() {
        "session_welcome" => Frame::Welcome {
            session: session["id"].as_str().unwrap_or_default().to_owned(),
            keepalive: session["keepalive_timeout_seconds"].as_u64().unwrap_or(10),
        },
        "session_keepalive" => Frame::Keepalive,
        "notification" => Frame::Notification {
            message_id: message.metadata.message_id,
            kind: message
                .metadata
                .subscription_type
                .or_else(|| payload["subscription"]["type"].as_str().map(str::to_owned))
                .unwrap_or_default(),
            event: payload["event"].clone(),
        },
        "session_reconnect" => {
            Frame::Reconnect { url: session["reconnect_url"].as_str().unwrap_or_default().to_owned() }
        }
        "revocation" => Frame::Revocation {
            kind: payload["subscription"]["type"].as_str().unwrap_or_default().to_owned(),
            status: payload["subscription"]["status"].as_str().unwrap_or_default().to_owned(),
        },
        other => Frame::Other(other.to_owned()),
    })
}

/// A subscription the adapter makes: its type and version, the scopes that allow it (any one), and its condition for
/// the broadcaster's user id.
#[derive(Clone, Copy, Debug)]
pub struct Topic {
    /// The subscription type.
    pub kind: &'static str,
    /// Its version.
    pub version: &'static str,
    /// The token needs one of these scopes; empty when none is needed.
    pub scopes: &'static [&'static str],
    /// The automation event it becomes.
    pub event: &'static str,
    condition: fn(&str) -> Value,
}

impl Topic {
    /// The subscription's condition for the broadcaster `user`.
    #[must_use]
    pub fn condition(&self, user: &str) -> Value {
        (self.condition)(user)
    }

    /// Whether a token with `scopes` may make this subscription.
    #[must_use]
    pub fn allowed(&self, scopes: &[String]) -> bool {
        self.scopes.is_empty() || self.scopes.iter().any(|s| scopes.iter().any(|t| t == s))
    }
}

fn broadcaster(user: &str) -> Value {
    json!({ "broadcaster_user_id": user })
}

fn broadcaster_and_moderator(user: &str) -> Value {
    json!({ "broadcaster_user_id": user, "moderator_user_id": user })
}

fn broadcaster_and_user(user: &str) -> Value {
    json!({ "broadcaster_user_id": user, "user_id": user })
}

fn raided(user: &str) -> Value {
    json!({ "to_broadcaster_user_id": user })
}

/// Every subscription the adapter makes, when the token allows it.
pub const TOPICS: [Topic; 6] = [
    Topic {
        kind: "channel.channel_points_custom_reward_redemption.add",
        version: "1",
        scopes: &["channel:read:redemptions", "channel:manage:redemptions"],
        event: Redeem::KIND,
        condition: broadcaster,
    },
    Topic {
        kind: "channel.chat.message",
        version: "1",
        scopes: &["user:read:chat"],
        event: ChatMessage::KIND,
        condition: broadcaster_and_user,
    },
    Topic {
        kind: "channel.follow",
        version: "2",
        scopes: &["moderator:read:followers"],
        event: Follow::KIND,
        condition: broadcaster_and_moderator,
    },
    Topic { kind: "channel.raid", version: "1", scopes: &[], event: Raid::KIND, condition: raided },
    Topic { kind: "channel.cheer", version: "1", scopes: &["bits:read"], event: Cheer::KIND, condition: broadcaster },
    Topic {
        kind: "channel.subscribe",
        version: "1",
        scopes: &["channel:read:subscriptions"],
        event: Sub::KIND,
        condition: broadcaster,
    },
];

fn text(event: &Value, key: &str) -> String {
    event[key].as_str().unwrap_or_default().to_owned()
}

fn number(event: &Value, key: &str) -> u32 {
    event[key].as_u64().and_then(|n| u32::try_from(n).ok()).unwrap_or(0)
}

/// The user named by the `<prefix>_id`, `<prefix>_login` and `<prefix>_name` fields, if the id is there.
fn user(event: &Value, prefix: &str) -> Option<ChatUser> {
    let id = event[format!("{prefix}_id")].as_str()?;
    let login = text(event, &format!("{prefix}_login"));
    let name = event[format!("{prefix}_name")].as_str().map_or_else(|| login.clone(), str::to_owned);
    Some(ChatUser::new(id, login, name))
}

/// The channel the event belongs to.
fn channel(event: &Value, prefix: &str) -> ChannelRef {
    let login = text(event, &format!("{prefix}_login"));
    let name = event[format!("{prefix}_name")].as_str().map_or_else(|| login.clone(), str::to_owned);
    ChannelRef::new(text(event, &format!("{prefix}_id")), name)
}

/// Roles from Twitch's chat badges: broadcaster, moderator, vip, and subscriber or founder.
fn roles(badges: &[Value]) -> Vec<Role> {
    let mut roles: Vec<Role> = badges
        .iter()
        .filter_map(|b| match b["set_id"].as_str()? {
            "broadcaster" => Some(Role::Broadcaster),
            "moderator" => Some(Role::Moderator),
            "vip" => Some(Role::Vip),
            "subscriber" | "founder" => Some(Role::Subscriber),
            _ => None,
        })
        .collect();
    roles.sort();
    roles.dedup();
    roles
}

/// The emote spans of a chat message's fragments, as byte offsets into the text they concatenate to.
fn emotes(fragments: &[Value]) -> Vec<EmoteSpan> {
    let mut at = 0u32;
    let mut spans = Vec::new();
    for fragment in fragments {
        let piece = fragment["text"].as_str().unwrap_or_default();
        let len = u32::try_from(piece.len()).unwrap_or(u32::MAX);
        if fragment["type"] == "emote" {
            let id = fragment["emote"]["id"].as_str().unwrap_or_default();
            spans.push(EmoteSpan::new(at, at.saturating_add(len), id, piece));
        }
        at = at.saturating_add(len);
    }
    spans
}

fn chat(twitch: Platform, event: &Value) -> Option<ChatMessage> {
    let badges = event["badges"].as_array().cloned().unwrap_or_default();
    let mut chatter = user(event, "chatter_user")?.with_roles(roles(&badges)).with_badges(
        badges.iter().filter_map(|b| Some(format!("{}/{}", b["set_id"].as_str()?, b["id"].as_str()?))).collect(),
    );
    if let Some(colour) = event["color"].as_str().filter(|c| !c.is_empty()) {
        chatter = chatter.with_colour(colour);
    }
    let message = &event["message"];
    let fragments = message["fragments"].as_array().cloned().unwrap_or_default();
    let mut chat = ChatMessage::new(
        twitch,
        channel(event, "broadcaster_user"),
        text(event, "message_id"),
        chatter,
        text(message, "text"),
    )
    .with_emotes(emotes(&fragments))
    .with_first_message(event["message_type"] == "user_intro");
    if let Some(parent) = event["reply"]["parent_message_id"].as_str() {
        chat = chat.with_reply_to(parent);
    }
    Some(chat)
}

/// A notification as an automation event, or `None` for a type this adapter doesn't subscribe to (or an event
/// missing the fields its kind needs). `message_id` becomes the origin id, so the inbox drops a redelivery.
#[must_use]
pub fn normalise(kind: &str, event: &Value, message_id: &str, at: Seconds) -> Option<LiveEvent> {
    let twitch = platform()?;
    let source = Source::platform(twitch.clone(), text(event, "broadcaster_user_login"));
    let live = match kind {
        "channel.channel_points_custom_reward_redemption.add" => {
            let reward = &event["reward"];
            let reward = Reward::new(text(reward, "id"), text(reward, "title"), number(reward, "cost"));
            let mut redeem = Redeem::new(
                twitch.clone(),
                channel(event, "broadcaster_user"),
                user(event, "user")?,
                reward,
                text(event, "id"),
            );
            if let Some(input) = event["user_input"].as_str().filter(|s| !s.is_empty()) {
                redeem = redeem.with_input(input);
            }
            LiveEvent::of(&redeem, source, at)
        }
        "channel.chat.message" => LiveEvent::of(&chat(twitch, event)?, source, at),
        "channel.follow" => LiveEvent::of(
            &Follow::new(twitch.clone(), channel(event, "broadcaster_user"), user(event, "user")?),
            source,
            at,
        ),
        "channel.raid" => {
            let to = channel(event, "to_broadcaster_user");
            let raid = Raid::new(twitch.clone(), to, user(event, "from_broadcaster_user")?, number(event, "viewers"));
            let source = Source::platform(twitch.clone(), text(event, "to_broadcaster_user_login"));
            LiveEvent::of(&raid, source, at)
        }
        "channel.cheer" => {
            let mut cheer = Cheer::new(
                twitch.clone(),
                channel(event, "broadcaster_user"),
                number(event, "bits"),
                text(event, "message"),
            );
            if event["is_anonymous"] != true
                && let Some(user) = user(event, "user")
            {
                cheer = cheer.with_user(user);
            }
            LiveEvent::of(&cheer, source, at)
        }
        "channel.subscribe" => {
            let sub = Sub::new(
                twitch.clone(),
                channel(event, "broadcaster_user"),
                user(event, "user")?,
                text(event, "tier"),
                1,
            );
            let sub = if event["is_gift"] == true { sub.with_gift(None) } else { sub };
            LiveEvent::of(&sub, source, at)
        }
        _ => return None,
    };
    live.ok().map(|e| e.with_origin_id(message_id))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_topic_becomes_a_registered_kind() {
        for topic in TOPICS {
            assert!(omuzaki_live_api::event::find_event_kind(topic.event).is_some(), "{}", topic.event);
        }
    }

    #[test]
    fn a_raid_needs_no_scope_and_a_redeem_needs_one() {
        let raid = TOPICS.iter().find(|t| t.kind == "channel.raid").copied();
        assert!(raid.is_some_and(|t| t.allowed(&[])));
        let redeem = TOPICS[0];
        assert!(!redeem.allowed(&["bits:read".to_owned()]));
        assert!(redeem.allowed(&["channel:manage:redemptions".to_owned()]));
    }

    #[test]
    fn emote_spans_are_byte_offsets() {
        let fragments = vec![
            json!({ "type": "text", "text": "hé " }),
            json!({ "type": "emote", "text": "Kappa", "emote": { "id": "25" } }),
        ];
        let spans = emotes(&fragments);
        assert_eq!(spans, vec![EmoteSpan::new(4, 9, "25", "Kappa")]);
    }
}
