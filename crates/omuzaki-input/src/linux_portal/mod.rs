//! The Wayland bindings backend (design §13 IN-9, §5 "Wayland"): the desktop's `GlobalShortcuts` portal, through ashpd.
//! On Wayland no program reads another's keys, so bindings go through the desktop: the binding table is registered
//! as the portal's shortcut list, one shortcut per key binding, described by the binding's name (never by a chord this
//! crate invents), and registered again whenever the table changes. The desktop agrees the triggers with the person;
//! its `Activated` and `Deactivated` signals become the binding's press and release, matched by id. The portal hands
//! over no keys, so this backend makes no motion and its source is never announced.
//!
//! The portal's own descriptions of the triggers (`trigger_description`) are dropped as the reply arrives: never
//! stored, logged, put in a state or shown. A portal that is missing or refuses fails this backend with a fixed
//! reason; the others keep running.
#![cfg(target_os = "linux")]

use std::collections::HashMap;
use std::time::Duration;

use ashpd::desktop::CreateSessionOptions;
use ashpd::desktop::global_shortcuts::{BindShortcutsOptions, GlobalShortcuts, NewShortcut};
use ashpd::zbus::Message;
use ashpd::zbus::zvariant::{OwnedObjectPath, OwnedValue};
use futures_util::StreamExt as _;
use omuzaki_input_api::binding::BindingId;
use omuzaki_input_api::capture::{BackendState, CaptureCaps, Support};
use omuzaki_input_api::event::{DeviceClass, DeviceMask};

use crate::hub::{Chosen, InputBackend, register_input_backend};
use crate::linux_x11::wayland;
use crate::matcher::Chord;
use crate::normalise::{RawEvent, RawSink};
use crate::shared::Table;

const PORTAL: InputBackend = InputBackend {
    name: "linux.portal",
    classes: DeviceMask::NONE.with(DeviceClass::Keyboard),
    caps,
    run: Some(run),
    chosen: Chosen::IfUnlessOptedIn(wayland),
};
register_input_backend!(PORTAL);

/// The handle the backend files its shortcuts under.
const SHORTCUTS: u64 = 1;
/// How often the session looks at the epochs and the table between signals; the idle call follows each look.
const POLL: Duration = Duration::from_millis(20);
/// How long the loop parks between looks while capture does not read the keyboard.
const PARK: Duration = Duration::from_millis(250);
/// A shortcut's id is the binding's, with this prefix.
const PREFIX: &str = "binding-";

/// The fixed reasons a portal fails with.
const MISSING: &str = "the desktop has no GlobalShortcuts portal";
const REFUSED: &str = "the GlobalShortcuts portal refused the shortcuts";
const GONE: &str = "the GlobalShortcuts portal went away";

fn caps() -> CaptureCaps {
    CaptureCaps {
        bindings: Support::Portal,
        typing: Support::NeedsOptIn,
        pointer: Support::NeedsOptIn,
        ..CaptureCaps::default()
    }
}

fn run(sink: RawSink) {
    run_on(sink, None);
}

/// The loop, on the session bus (at `address` instead, for the tests): one portal session per run.
pub(crate) fn run_on(mut sink: RawSink, address: Option<&str>) {
    let Ok(runtime) = tokio::runtime::Builder::new_current_thread().enable_all().build() else {
        sink.set_state(BackendState::failed("the portal's runtime could not start"));
        return;
    };
    loop {
        if !sink.wait_reading(PARK) {
            return;
        }
        if !sink.reading() {
            continue;
        }
        if let Err(reason) = runtime.block_on(session(&mut sink, address)) {
            sink.set_state(BackendState::failed(reason));
            return;
        }
    }
}

/// One run: a portal session with the table bound, handing its signals on until the run ends.
async fn session(sink: &mut RawSink, address: Option<&str>) -> Result<(), &'static str> {
    let portal = match address {
        Some(address) => {
            let connection = ashpd::zbus::connection::Builder::address(address)
                .map_err(|_| MISSING)?
                .build()
                .await
                .map_err(|_| MISSING)?;
            GlobalShortcuts::with_connection(connection).await
        }
        None => GlobalShortcuts::new().await,
    }
    .map_err(|_| MISSING)?;
    // One stream of every signal the portal sends, so a release never overtakes its press.
    let mut signals = portal.receive_all_signals().await.map_err(|_| MISSING)?;
    let session = portal.create_session(CreateSessionOptions::default()).await.map_err(|_| MISSING)?;
    let run = sink.epochs();
    let mut bound = None;
    let mut tick = tokio::time::interval(POLL);
    let outcome = loop {
        if !sink.reading() || sink.epochs() != run {
            break Ok(());
        }
        let table = sink.table();
        if bound != Some(table.generation) {
            let shortcuts = shortcuts(&table);
            let request = portal.bind_shortcuts(&session, &shortcuts, None, BindShortcutsOptions::default()).await;
            // The reply names each trigger as the desktop shows it: dropped here, unread.
            if request.and_then(|r| r.response().map(drop)).is_err() {
                break Err(REFUSED);
            }
            bound = Some(table.generation);
        }
        let shortcut = tokio::select! {
            signal = signals.next() => match signal {
                Some(signal) => shortcut_of(&signal),
                None => break Err(GONE),
            },
            _ = tick.tick() => None,
        };
        if let Some((id, down)) = shortcut
            && sink.epochs() == run
        {
            sink.push(RawEvent::Shortcut { device: SHORTCUTS, class: DeviceClass::Keyboard, id, down });
        }
        sink.idle();
    };
    let _closed = session.close().await;
    outcome
}

/// The shortcut list for `table`: its key bindings (a pad binding is the gamepad backend's), each by its name.
fn shortcuts(table: &Table) -> Vec<NewShortcut> {
    table
        .entries
        .iter()
        .filter(|e| !matches!(e.chord, Some(Chord::Pad { .. })))
        .map(|e| NewShortcut::new(format!("{PREFIX}{}", e.id.0), e.name.clone()))
        .collect()
}

/// The binding an `Activated` (down) or `Deactivated` signal names, or `None` for any other signal or a shortcut this
/// backend never registered. The signal's options are not read.
fn shortcut_of(signal: &Message) -> Option<(BindingId, bool)> {
    let header = signal.header();
    let down = match header.member()?.as_str() {
        "Activated" => true,
        "Deactivated" => false,
        _ => return None,
    };
    let (_session, shortcut, _at, _options): (OwnedObjectPath, String, u64, HashMap<String, OwnedValue>) =
        signal.body().deserialize().ok()?;
    binding_of(&shortcut).map(|id| (id, down))
}

/// The binding a shortcut id names, or `None` for one this backend never registered.
fn binding_of(shortcut: &str) -> Option<BindingId> {
    shortcut.strip_prefix(PREFIX)?.parse().ok().map(BindingId)
}

#[cfg(test)]
mod tests {
    use omuzaki_input_api::binding::BindingAction;

    use super::*;
    use crate::matcher::Mods;
    use crate::normalise::HidUsage;
    use crate::shared::TableEntry;

    #[test]
    fn a_shortcut_id_names_its_binding_and_nothing_else() {
        assert_eq!(binding_of("binding-7"), Some(BindingId(7)));
        assert_eq!(binding_of("binding-"), None);
        assert_eq!(binding_of("other-7"), None);
        assert_eq!(binding_of("binding-x"), None);
    }

    #[test]
    fn only_the_two_shortcut_signals_are_read() {
        let read = |member: &str, shortcut: &str| {
            let session = OwnedObjectPath::try_from("/session").unwrap();
            let body = (session, shortcut.to_owned(), 5u64, HashMap::<String, OwnedValue>::new());
            let signal = Message::signal("/org/freedesktop/portal/desktop", "org.freedesktop.portal.GlobalShortcuts", member)
                .unwrap()
                .build(&body)
                .unwrap();
            shortcut_of(&signal)
        };
        assert_eq!(read("Activated", "binding-3"), Some((BindingId(3), true)));
        assert_eq!(read("Deactivated", "binding-3"), Some((BindingId(3), false)));
        assert_eq!(read("ShortcutsChanged", "binding-3"), None);
        assert_eq!(read("Activated", "someone-else"), None);
    }

    #[test]
    fn the_shortcut_list_is_the_key_bindings_by_name() {
        let entry = |id, name: &str, chord| TableEntry {
            id: BindingId(id),
            name: name.to_owned(),
            chord,
            action: BindingAction::Trigger(0),
        };
        let table = Table {
            generation: 3,
            entries: vec![
                entry(1, "Wave", Some(Chord::Key { usage: HidUsage(0x04), mods: Mods(0) })),
                entry(2, "Jump", Some(Chord::Pad { buttons: 1 })),
                entry(3, "Hush", None),
            ],
        };
        let list = shortcuts(&table);
        assert_eq!(list.len(), 2, "the pad binding is not the portal's");
    }
}
