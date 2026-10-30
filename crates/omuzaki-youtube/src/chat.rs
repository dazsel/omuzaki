//! YouTube's live chat as data, and its normalisation into the show's events. Written from the public YouTube Data
//! API v3 reference (`liveChatMessages`, `liveBroadcasts`, `videos`, `subscriptions`).
//!
//! | YouTube | Event |
//! |---|---|
//! | `textMessageEvent` | `chat.message` |
//! | `superChatEvent`, `superStickerEvent` | `tip` |
//! | `newSponsorEvent` (a new member, or an upgrade) | `sub` (1 month) |
//! | `memberMilestoneChatEvent` | `sub` (its month count, its comment) |
//! | `membershipGiftingEvent` | `sub_gift` |
//! | `giftMembershipReceivedEvent` | `sub` with `gift` |
//! | a new subscriber (`subscriptions` with `myRecentSubscribers`) | `follow` |
//!
//! YouTube's channel owner is a broadcaster, its moderators moderators, and its members subscribers.

use omuzaki_core::time::Seconds;
use omuzaki_live_api::event::payloads::{ChannelRef, ChatMessage, ChatUser, Follow, Money, Role, Sub, SubGift, Tip};
use omuzaki_live_api::event::{LiveEvent, Source};
use omuzaki_live_api::ids::Platform;
use serde::Deserialize;
use serde_json::Value;

/// The platform id every YouTube event carries.
pub const PLATFORM: &str = "youtube";

/// The platform as an id (always `Some`: `youtube` is a valid id).
#[must_use]
pub fn platform() -> Option<Platform> {
    Platform::new(PLATFORM).ok()
}

/// One page of `liveChatMessages.list`.
#[derive(Clone, Debug, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Page {
    /// Where the next poll starts.
    #[serde(default)]
    pub next_page_token: Option<String>,
    /// How long YouTube asks the next poll to wait, milliseconds.
    #[serde(default)]
    pub polling_interval_millis: Option<u64>,
    /// Set once the stream has gone offline.
    #[serde(default)]
    pub offline_at: Option<String>,
    /// The messages.
    #[serde(default)]
    pub items: Vec<Value>,
}

/// The live chat the adapter reads: its id and whose channel it is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Chat {
    /// `liveChatId`.
    pub id: String,
    /// The broadcasting channel's id.
    pub channel_id: String,
    /// The broadcast's title.
    pub title: String,
}

fn text<'a>(v: &'a Value, key: &str) -> &'a str {
    v.get(key).and_then(Value::as_str).unwrap_or_default()
}

/// The author of an item as a chat user with their roles.
fn author(item: &Value) -> Option<ChatUser> {
    let a = item.get("authorDetails")?;
    let id = text(a, "channelId");
    if id.is_empty() {
        return None;
    }
    let name = text(a, "displayName");
    let mut roles = Vec::new();
    let flag = |key: &str| a.get(key).and_then(Value::as_bool).unwrap_or(false);
    if flag("isChatOwner") {
        roles.push(Role::Broadcaster);
    }
    if flag("isChatModerator") {
        roles.push(Role::Moderator);
    }
    if flag("isChatSponsor") {
        roles.push(Role::Subscriber);
    }
    // YouTube handles are `@name`; the login is the handle without its sign, or the display name when there is none.
    let login = name.trim_start_matches('@').to_lowercase().replace(' ', "");
    Some(ChatUser::new(id, login, name).with_roles(roles))
}

/// Micros of a currency from a Super Chat or Super Sticker's details.
fn money(details: &Value) -> Money {
    let micros = details
        .get("amountMicros")
        .and_then(|v| v.as_str().and_then(|s| s.parse().ok()).or_else(|| v.as_i64()))
        .unwrap_or(0);
    Money::new(micros, text(details, "currency"))
}

/// The event a chat item makes, or `None` for one the show has no kind for (deletions, bans, polls).
pub fn normalise(item: &Value, chat: &Chat, at: Seconds) -> Option<LiveEvent> {
    let youtube = platform()?;
    let snippet = item.get("snippet")?;
    let id = text(item, "id");
    let channel = ChannelRef::new(chat.channel_id.clone(), chat.title.clone());
    let source = Source::platform(youtube.clone(), chat.channel_id.clone());
    let event = match text(snippet, "type") {
        "textMessageEvent" => {
            let words = snippet
                .pointer("/textMessageDetails/messageText")
                .and_then(Value::as_str)
                .unwrap_or_else(|| text(snippet, "displayMessage"));
            LiveEvent::of(&ChatMessage::new(youtube, channel, id, author(item)?, words), source, at)
        }
        "superChatEvent" => {
            let d = snippet.get("superChatDetails")?;
            let tip = Tip::new(youtube, channel, author(item)?, money(d)).with_message(text(d, "userComment"));
            LiveEvent::of(&tip, source, at)
        }
        "superStickerEvent" => {
            let d = snippet.get("superStickerDetails")?;
            let sticker = d.pointer("/superStickerMetadata/altText").and_then(Value::as_str).unwrap_or_default();
            LiveEvent::of(&Tip::new(youtube, channel, author(item)?, money(d)).with_message(sticker), source, at)
        }
        "newSponsorEvent" => {
            let level =
                snippet.pointer("/newSponsorDetails/memberLevelName").and_then(Value::as_str).unwrap_or("member");
            LiveEvent::of(&Sub::new(youtube, channel, author(item)?, level, 1), source, at)
        }
        "memberMilestoneChatEvent" => {
            let d = snippet.get("memberMilestoneChatDetails")?;
            let months = d.get("memberMonth").and_then(Value::as_u64).and_then(|m| u32::try_from(m).ok()).unwrap_or(1);
            let level = d.get("memberLevelName").and_then(Value::as_str).unwrap_or("member");
            let sub = Sub::new(youtube, channel, author(item)?, level, months).with_message(text(d, "userComment"));
            LiveEvent::of(&sub, source, at)
        }
        "membershipGiftingEvent" => {
            let d = snippet.get("membershipGiftingDetails")?;
            let count = d.get("giftMembershipsCount").and_then(Value::as_u64).and_then(|m| u32::try_from(m).ok())?;
            let level = d.get("giftMembershipsLevelName").and_then(Value::as_str).unwrap_or("member");
            let mut gift = SubGift::new(youtube, channel, count, level);
            if let Some(gifter) = author(item) {
                gift = gift.with_gifter(gifter);
            }
            LiveEvent::of(&gift, source, at)
        }
        "giftMembershipReceivedEvent" => {
            let level = snippet
                .pointer("/giftMembershipReceivedDetails/memberLevelName")
                .and_then(Value::as_str)
                .unwrap_or("member");
            LiveEvent::of(&Sub::new(youtube, channel, author(item)?, level, 1).with_gift(None), source, at)
        }
        _ => return None,
    };
    event.ok().map(|e| e.with_origin_id(id))
}

/// The `follow` a new subscriber makes, from an item of `subscriptions.list` with `myRecentSubscribers`.
pub fn subscriber(item: &Value, chat: &Chat, at: Seconds) -> Option<LiveEvent> {
    let youtube = platform()?;
    let s = item.get("subscriberSnippet")?;
    let id = text(s, "channelId");
    if id.is_empty() {
        return None;
    }
    let name = text(s, "title");
    let user = ChatUser::new(id, name.to_lowercase().replace(' ', ""), name);
    let channel = ChannelRef::new(chat.channel_id.clone(), chat.title.clone());
    let source = Source::platform(youtube.clone(), chat.channel_id.clone());
    LiveEvent::of(&Follow::new(youtube, channel, user), source, at).ok().map(|e| e.with_origin_id(text(item, "id")))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn chat() -> Chat {
        Chat { id: "chat1".into(), channel_id: "UCme".into(), title: "Cosy stream".into() }
    }

    fn item(kind: &str, details: (&str, Value), author: &Value) -> Value {
        let mut snippet = json!({ "type": kind, "displayMessage": "shown" });
        snippet[details.0] = details.1;
        json!({ "id": "m1", "snippet": snippet, "authorDetails": author })
    }

    fn viewer() -> Value {
        json!({ "channelId": "UCviewer", "displayName": "@Mochi", "isChatSponsor": true, "isChatModerator": false })
    }

    #[test]
    fn a_chat_message_carries_the_author_and_their_roles() {
        let i = item("textMessageEvent", ("textMessageDetails", json!({ "messageText": "hi!" })), &viewer());
        let e = normalise(&i, &chat(), Seconds(1.0)).unwrap();
        assert_eq!(e.kind.as_str(), "chat.message");
        assert_eq!(e.payload["text"], "hi!");
        assert_eq!(e.payload["user"]["name"], "@Mochi");
        assert_eq!(e.payload["user"]["login"], "mochi");
        assert_eq!(e.payload["user"]["roles"], json!(["subscriber"]));
        assert_eq!(e.origin_id.as_deref(), Some("m1"));
    }

    #[test]
    fn a_super_chat_is_a_tip_in_its_currency() {
        let d = json!({ "amountMicros": "5000000", "currency": "USD", "userComment": "for the outfit" });
        let e = normalise(&item("superChatEvent", ("superChatDetails", d), &viewer()), &chat(), Seconds(0.0)).unwrap();
        assert_eq!(e.kind.as_str(), "tip");
        assert_eq!(e.payload["amount"], json!({ "micros": 5_000_000, "currency": "USD" }));
        assert_eq!(e.payload["message"], "for the outfit");
    }

    #[test]
    fn memberships_are_subs() {
        let d = json!({ "memberMonth": 7, "memberLevelName": "Stars", "userComment": "7 months!" });
        let e = normalise(
            &item("memberMilestoneChatEvent", ("memberMilestoneChatDetails", d), &viewer()),
            &chat(),
            Seconds(0.0),
        )
        .unwrap();
        assert_eq!(
            (e.kind.as_str(), e.payload["months"].clone(), e.payload["tier"].clone()),
            ("sub", json!(7), json!("Stars"))
        );
        let d = json!({ "giftMembershipsCount": 5, "giftMembershipsLevelName": "Stars" });
        let e = normalise(
            &item("membershipGiftingEvent", ("membershipGiftingDetails", d), &viewer()),
            &chat(),
            Seconds(0.0),
        )
        .unwrap();
        assert_eq!((e.kind.as_str(), e.payload["count"].clone()), ("sub_gift", json!(5)));
        let e = normalise(
            &item("giftMembershipReceivedEvent", ("giftMembershipReceivedDetails", json!({})), &viewer()),
            &chat(),
            Seconds(0.0),
        )
        .unwrap();
        assert_eq!((e.kind.as_str(), e.payload["gift"].clone()), ("sub", json!(true)));
    }

    #[test]
    fn kinds_the_show_has_no_event_for_are_skipped() {
        let i = item("messageDeletedEvent", ("messageDeletedDetails", json!({ "deletedMessageId": "x" })), &viewer());
        assert!(normalise(&i, &chat(), Seconds(0.0)).is_none());
    }

    #[test]
    fn a_new_subscriber_is_a_follow() {
        let i = json!({ "id": "s1", "subscriberSnippet": { "channelId": "UCnew", "title": "Pip Pop" } });
        let e = subscriber(&i, &chat(), Seconds(0.0)).unwrap();
        assert_eq!((e.kind.as_str(), e.payload["user"]["name"].clone()), ("follow", json!("Pip Pop")));
    }
}
