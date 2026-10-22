//! IN-9's acceptance (design §13): the Wayland portal backend against a fake `GlobalShortcuts` portal on a private
//! D-Bus session bus, started by the test (skipped with a message where the runner has no `dbus-daemon`).
#![cfg(target_os = "linux")]
#![allow(clippy::unwrap_used)]
// The fake portal's methods take what the portal's do, whether they read it or not.
#![allow(clippy::unused_self, clippy::used_underscore_binding)]

mod support;

use std::collections::HashMap;
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ashpd::zbus;
use ashpd::zbus::message::Header;
use ashpd::zbus::names::BusName;
use ashpd::zbus::zvariant::{OwnedObjectPath, OwnedValue, Value};
use ashpd::zbus::{Connection, connection};
use omuzaki_input_api::binding::{BindingAction, BindingId, Edge};
use omuzaki_input_api::capture::{BackendState, CaptureState};
use omuzaki_input_api::event::{DeviceClass, DeviceMask};
use support::Rig;

/// The longest a test waits: a ceiling for a failing test, never an ordering.
const CEILING: Duration = Duration::from_secs(10);
/// The desk test backend runs the portal loop.
const DESK_BACKEND: usize = 0;
/// What the fake portal says a trigger is: it must never surface.
const TRIGGER: &str = "TRIGGER-DESCRIPTION-Ctrl+Alt+Q";
const DESKTOP: &str = "/org/freedesktop/portal/desktop";
const INTERFACE: &str = "org.freedesktop.portal.GlobalShortcuts";

/// A private session bus for one test, stopped when dropped.
struct Bus {
    child: Child,
    address: String,
}

impl Drop for Bus {
    fn drop(&mut self) {
        let _killed = self.child.kill();
        let _reaped = self.child.wait();
    }
}

fn bus() -> Option<Bus> {
    let child = Command::new("dbus-daemon")
        .args(["--session", "--nofork", "--print-address"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn();
    let Ok(mut child) = child else {
        eprintln!("skipped: no dbus-daemon on this runner");
        return None;
    };
    let mut line = String::new();
    BufReader::new(child.stdout.take().unwrap()).read_line(&mut line).unwrap();
    Some(Bus { child, address: line.trim().to_owned() })
}

fn until(mut f: impl FnMut() -> bool) -> bool {
    let start = Instant::now();
    while start.elapsed() < CEILING {
        if f() {
            return true;
        }
        std::thread::park_timeout(Duration::from_millis(2));
    }
    false
}

/// One registered shortcut as the fake saw it: its id, the option keys it came with, and its description.
type Seen = (String, Vec<String>, String);

#[derive(Default)]
struct State {
    sessions: Vec<String>,
    binds: Vec<Vec<Seen>>,
    refuse: bool,
}

struct Shortcuts {
    state: Arc<Mutex<State>>,
}

/// The caller's request or session path, as the portal documentation derives it.
fn path(header: &Header<'_>, kind: &str, options: &HashMap<String, OwnedValue>, key: &str) -> String {
    let sender = header.sender().map(|s| s.as_str().trim_start_matches(':').replace('.', "_")).unwrap_or_default();
    let token = options.get(key).and_then(|v| String::try_from(v.try_clone().unwrap()).ok()).unwrap_or_default();
    format!("{DESKTOP}/{kind}/{sender}/{token}")
}

async fn respond(connection: &Connection, request: &str, code: u32, results: HashMap<&str, Value<'_>>) {
    connection
        .emit_signal(None::<BusName<'_>>, request, "org.freedesktop.portal.Request", "Response", &(code, results))
        .await
        .unwrap();
}

#[ashpd::zbus::interface(name = "org.freedesktop.portal.GlobalShortcuts", crate = "ashpd::zbus")]
impl Shortcuts {
    #[zbus(property)]
    fn version(&self) -> u32 {
        1
    }

    async fn create_session(
        &self,
        options: HashMap<String, OwnedValue>,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] connection: &Connection,
    ) -> OwnedObjectPath {
        let request = path(&header, "request", &options, "handle_token");
        let session = path(&header, "session", &options, "session_handle_token");
        self.state.lock().unwrap().sessions.push(session.clone());
        respond(connection, &request, 0, HashMap::from([("session_handle", Value::from(session.as_str()))])).await;
        OwnedObjectPath::try_from(request).unwrap()
    }

    async fn bind_shortcuts(
        &self,
        _session: OwnedObjectPath,
        shortcuts: Vec<(String, HashMap<String, OwnedValue>)>,
        _parent_window: String,
        options: HashMap<String, OwnedValue>,
        #[zbus(header)] header: Header<'_>,
        #[zbus(connection)] connection: &Connection,
    ) -> OwnedObjectPath {
        let request = path(&header, "request", &options, "handle_token");
        let seen: Vec<Seen> = shortcuts
            .iter()
            .map(|(id, info)| {
                let mut keys: Vec<String> = info.keys().cloned().collect();
                keys.sort();
                let description =
                    info.get("description").and_then(|v| String::try_from(v.try_clone().unwrap()).ok()).unwrap();
                (id.clone(), keys, description)
            })
            .collect();
        let refuse = {
            let mut state = self.state.lock().unwrap();
            state.binds.push(seen.clone());
            state.refuse
        };
        if refuse {
            respond(connection, &request, 1, HashMap::new()).await;
        } else {
            let bound: Vec<(String, HashMap<&str, Value<'_>>)> = seen
                .into_iter()
                .map(|(id, _, description)| {
                    let info = HashMap::from([
                        ("description", Value::from(description)),
                        ("trigger_description", Value::from(TRIGGER)),
                    ]);
                    (id, info)
                })
                .collect();
            respond(connection, &request, 0, HashMap::from([("shortcuts", Value::from(bound))])).await;
        }
        OwnedObjectPath::try_from(request).unwrap()
    }
}

/// The fake portal: its own connection on the bus, owning the portal's name, on a runtime of its own.
struct Portal {
    runtime: tokio::runtime::Runtime,
    connection: Connection,
    state: Arc<Mutex<State>>,
}

impl Portal {
    fn start(bus: &Bus, refuse: bool) -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread().worker_threads(1).enable_all().build().unwrap();
        let state = Arc::new(Mutex::new(State { refuse, ..State::default() }));
        let shortcuts = Shortcuts { state: Arc::clone(&state) };
        let connection = runtime.block_on(async {
            connection::Builder::address(bus.address.as_str())
                .unwrap()
                .name("org.freedesktop.portal.Desktop")
                .unwrap()
                .serve_at(DESKTOP, shortcuts)
                .unwrap()
                .build()
                .await
                .unwrap()
        });
        Self { runtime, connection, state }
    }

    fn binds(&self) -> Vec<Vec<Seen>> {
        self.state.lock().unwrap().binds.clone()
    }

    /// The desktop says binding `id`'s shortcut went active or inactive.
    fn signal(&self, id: BindingId, active: bool) {
        let session = self.state.lock().unwrap().sessions.last().cloned().unwrap();
        let session = OwnedObjectPath::try_from(session).unwrap();
        let name = if active { "Activated" } else { "Deactivated" };
        let body = (session, format!("binding-{}", id.0), 0u64, HashMap::<&str, Value<'_>>::new());
        self.runtime
            .block_on(self.connection.emit_signal(None::<BusName<'_>>, DESKTOP, INTERFACE, name, &body))
            .unwrap();
    }
}

fn hits_until(rig: &mut Rig, count: usize) -> Vec<(BindingId, BindingAction, Edge)> {
    let mut all = Vec::new();
    assert!(
        until(|| {
            all.extend(rig.hits());
            all.len() >= count
        }),
        "{all:?}"
    );
    all
}

fn backend_state(rig: &Rig) -> Option<BackendState> {
    rig.control.report().backends.into_iter().find(|b| b.name == "a.desk").map(|b| b.state)
}

#[test]
fn bindings_register_by_name_and_the_portal_signals_press_and_release_them() {
    let Some(bus) = bus() else { return };
    let portal = Portal::start(&bus, false);
    let mut rig = Rig::new();
    let names = ["Wave", "Nod", "Hush"];
    let ids: Vec<BindingId> = names
        .iter()
        .zip(0u8..)
        .map(|(name, slot)| rig.control.add_binding(name, DeviceClass::Keyboard, BindingAction::Trigger(slot)).unwrap())
        .collect();
    let _thread = rig.hub.run_portal(DESK_BACKEND, &bus.address).unwrap();
    rig.indicator.beat();
    rig.control.start(DeviceMask::NONE.with(DeviceClass::Keyboard)).unwrap();
    assert!(until(|| !portal.binds().is_empty()), "the table was registered");

    // Three shortcuts, by name, and nothing but the description: no key code, no trigger we invented.
    let first = portal.binds().remove(0);
    let expected: Vec<Seen> = ids
        .iter()
        .zip(names)
        .map(|(id, name)| (format!("binding-{}", id.0), vec!["description".to_owned()], name.to_owned()))
        .collect();
    assert_eq!(first, expected);

    // The signals are the bindings' presses and releases, resolved to the right binding.
    portal.signal(ids[1], true);
    portal.signal(ids[1], false);
    let hits = hits_until(&mut rig, 2);
    assert_eq!(
        hits,
        [(ids[1], BindingAction::Trigger(1), Edge::Press), (ids[1], BindingAction::Trigger(1), Edge::Release)]
    );

    // A burst keeps its order: every release follows its own press.
    for _ in 0..10 {
        portal.signal(ids[0], true);
        portal.signal(ids[0], false);
    }
    let edges: Vec<Edge> = hits_until(&mut rig, 20).into_iter().map(|(_, _, edge)| edge).collect();
    assert_eq!(edges, [Edge::Press, Edge::Release].repeat(10));

    // A new table is registered again, and its bindings resolve under its generation.
    let added = rig.control.add_binding("Blink", DeviceClass::Keyboard, BindingAction::Trigger(3)).unwrap();
    assert!(until(|| portal.binds().len() == 2), "registered again");
    assert_eq!(portal.binds()[1].len(), 4);
    portal.signal(added, true);
    portal.signal(added, false);
    let hits = hits_until(&mut rig, 2);
    assert_eq!(
        hits,
        [(added, BindingAction::Trigger(3), Edge::Press), (added, BindingAction::Trigger(3), Edge::Release)]
    );

    // A shortcut has no motion, and the desktop's trigger text is nowhere.
    let (motion, _) = rig.motion();
    assert!(motion.is_empty(), "{motion:?}");
    let report = serde_json::to_string(&rig.control.report()).unwrap();
    let bindings = format!("{:?}", rig.control.bindings());
    for text in [report, bindings, format!("{hits:?}")] {
        assert!(!text.contains("TRIGGER-DESCRIPTION"), "{text}");
    }
    assert!(matches!(rig.state(), CaptureState::On { .. }), "{:?}", rig.state());
}

#[test]
fn a_missing_portal_fails_only_its_backend() {
    let Some(bus) = bus() else { return };
    let rig = Rig::new();
    let _thread = rig.hub.run_portal(DESK_BACKEND, &bus.address).unwrap();
    rig.indicator.beat();
    rig.control.start(DeviceMask::NONE.with(DeviceClass::Keyboard).with(DeviceClass::Gamepad)).unwrap();
    let missing = BackendState::failed("the desktop has no GlobalShortcuts portal");
    assert!(until(|| backend_state(&rig) == Some(missing.clone())), "{:?}", backend_state(&rig));
    // The pads still run: capture is on, with the keyboard degraded.
    let CaptureState::On { classes, degraded, .. } = rig.state() else { panic!("{:?}", rig.state()) };
    assert_eq!(classes, DeviceMask::NONE.with(DeviceClass::Gamepad));
    assert_eq!(degraded, DeviceMask::NONE.with(DeviceClass::Keyboard));
}

#[test]
fn a_portal_that_refuses_fails_with_a_fixed_reason_and_alone_fails_capture() {
    let Some(bus) = bus() else { return };
    let portal = Portal::start(&bus, true);
    let rig = Rig::new();
    let _id = rig.control.add_binding("Wave", DeviceClass::Keyboard, BindingAction::Trigger(0)).unwrap();
    let _thread = rig.hub.run_portal(DESK_BACKEND, &bus.address).unwrap();
    rig.indicator.beat();
    rig.control.start(DeviceMask::NONE.with(DeviceClass::Keyboard)).unwrap();
    let refused = BackendState::failed("the GlobalShortcuts portal refused the shortcuts");
    assert!(until(|| backend_state(&rig) == Some(refused.clone())), "{:?}", backend_state(&rig));
    assert_eq!(portal.binds().len(), 1);
    assert!(matches!(rig.state(), CaptureState::Failed { .. }), "the portal was the only backend: {:?}", rig.state());
}
