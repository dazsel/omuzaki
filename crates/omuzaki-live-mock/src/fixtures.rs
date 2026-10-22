//! Synthetic events for tests: invented users, invented messages, one invented channel. Recorded chat from a real
//! stream is never a fixture (design D16).
//!
//! Every event is at time 0 from [`PLATFORM`] in the [`channel`]; set [`at`] for another time. Ids are derived from
//! the arguments, so the same call always gives the same event.

use omuzaki_core::time::Seconds;
use omuzaki_live_api::event::payloads::{
    ChannelRef, ChatMessage, ChatUser, Cheer, Follow, Money, Raid, Redeem, Reward, Role, Sub, Tip,
};
use omuzaki_live_api::event::{EventPayload, LiveEvent, Source};
use omuzaki_live_api::ids::{EventKind, Platform};
use omuzaki_live_api::node::rng_stream;
use serde_json::Value;

/// The platform every fixture comes from.
pub const PLATFORM: &str = "twitch";

/// Invented display names, with Japanese, Korean and emoji ones for the text paths that must not care.
pub const NAMES: [&str; 6] = ["Kiri", "Mochi", "Pell", "ことり", "하늘", "Juno ✨"];

/// The invented channel every fixture belongs to.
pub fn channel() -> ChannelRef {
    ChannelRef::new("chan-0001", "Synthetic Studio")
}

fn platform() -> Platform {
    match Platform::new(PLATFORM) {
        Ok(platform) => platform,
        Err(error) => panic!("the fixture platform is a slug: {error}"),
    }
}

/// An id derived from its parts, so fixtures are deterministic.
fn derived(prefix: &str, a: &str, b: &str) -> String {
    format!("{prefix}-{:016x}", rng_stream(a, b))
}

/// The user `name`: a stable id and handle derived from the name, no roles.
pub fn user(name: &str) -> ChatUser {
    let login: String = name.to_lowercase().split_whitespace().collect::<Vec<_>>().join("_");
    ChatUser::new(derived("user", name, ""), login, name)
}

/// The user `name` with `roles`.
pub fn user_with(name: &str, roles: impl IntoIterator<Item = Role>) -> ChatUser {
    user(name).with_roles(roles)
}

fn event<P: EventPayload>(payload: &P) -> LiveEvent {
    match LiveEvent::of(payload, Source::platform(platform(), channel().id), Seconds(0.0)) {
        Ok(event) => event,
        Err(error) => panic!("a fixture's payload is always valid: {error}"),
    }
}

/// `event` at `at` on the runner clock.
pub fn at(mut event: LiveEvent, at: Seconds) -> LiveEvent {
    event.at = at;
    event
}

/// `name` says `text` in chat.
pub fn chat(name: &str, text: &str) -> LiveEvent {
    chat_from(user(name), text)
}

/// `user` says `text` in chat.
pub fn chat_from(user: ChatUser, text: &str) -> LiveEvent {
    let id = derived("msg", &user.id, text);
    event(&ChatMessage::new(platform(), channel(), id.clone(), user, text)).with_origin_id(id)
}

/// `name` redeems the reward titled `title` (cost 100).
pub fn redeem(name: &str, title: &str) -> LiveEvent {
    let reward = Reward::new(derived("reward", title, ""), title, 100);
    let id = derived("redemption", name, title);
    event(&Redeem::new(platform(), channel(), user(name), reward, id.clone())).with_origin_id(id)
}

/// `name` cheers `bits` bits.
pub fn cheer(name: &str, bits: u32) -> LiveEvent {
    event(&Cheer::new(platform(), channel(), bits, format!("Cheer{bits}")).with_user(user(name)))
}

/// The channel `from` raids with `viewers` viewers.
pub fn raid(from: &str, viewers: u32) -> LiveEvent {
    event(&Raid::new(platform(), channel(), user(from), viewers))
}

/// `name` follows.
pub fn follow(name: &str) -> LiveEvent {
    event(&Follow::new(platform(), channel(), user(name)))
}

/// `name` subscribes at `tier` for the first month.
pub fn sub(name: &str, tier: &str) -> LiveEvent {
    event(&Sub::new(platform(), channel(), user(name), tier, 1))
}

/// `name` tips `micros` millionths of `currency`.
pub fn tip(name: &str, micros: i64, currency: &str) -> LiveEvent {
    event(&Tip::new(platform(), channel(), user(name), Money::new(micros, currency)))
}

/// A `custom.<name>` event with `payload` (an object), from a test.
///
/// # Panics
/// If `name` doesn't make a valid event kind.
pub fn custom(name: &str, payload: Value) -> LiveEvent {
    match EventKind::new(format!("{}{name}", EventKind::CUSTOM_PREFIX)) {
        Ok(kind) => LiveEvent::new(kind, Source::Test, Seconds(0.0), payload),
        Err(error) => panic!("custom.{name}: {error}"),
    }
}

#[cfg(test)]
mod tests {
    use omuzaki_live_api::event::check_payload;

    use super::*;

    #[test]
    fn every_fixture_fits_its_kind_and_is_deterministic() {
        let events = [
            chat("Kiri", "!lurk"),
            redeem("Mochi", "Bonk"),
            cheer("Pell", 100),
            raid("ことり", 42),
            follow("하늘"),
            sub("Juno ✨", "1000"),
            tip("Kiri", 5_000_000, "USD"),
            custom("bonk", serde_json::json!({"n": 1})),
        ];
        for event in &events {
            assert_eq!(check_payload(&event.kind, &event.payload), Ok(()), "{}", event.kind);
        }
        assert_eq!(chat("Kiri", "hi"), chat("Kiri", "hi"));
        assert_ne!(chat("Kiri", "hi").origin_id, chat("Kiri", "ho").origin_id);
        assert_eq!(user("Juno ✨").login, "juno_✨");
    }
}
