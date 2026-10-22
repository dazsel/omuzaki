//! `output.recommend`: the first-run probe for the output this machine should use (`docs/design/first-run.md` §5;
//! #1048). A query in the `read` scope with no side effects: it reads the registered output kinds' availability, the
//! output window's capabilities, and whether OBS is running.
//!
//! With OBS running, the best way into OBS wins: a GPU texture sender with alpha (Spout, Syphon, NDI), else the output
//! window (transparent where the platform allows, keyed otherwise), else the virtual camera. Without OBS, the virtual
//! camera wins, since every video app (Zoom, Discord, Meet, browsers) opens it, then the window. The answer names the
//! others in the same order, says why each unavailable kind is, and gives one line of OBS steps for the pick.
//!
//! Whether OBS runs is asked of an [`ObsProbe`] service when the app provides one, else of [`LocalObs`]: a TCP connect
//! to OBS WebSocket's default port on this computer, closed at once without a handshake (no login, nothing sent).

use std::net::{Ipv4Addr, SocketAddr, TcpStream};
use std::time::Duration;

use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use omuzaki_stream_api::output::{ObsProbe, output_kinds};
use omuzaki_stream_api::window::{WindowCaps, WindowHost};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// The output window's kind name in a recommendation.
pub const WINDOW: &str = "window";
/// OBS WebSocket's default port.
pub const OBS_PORT: u16 = 4455;
/// How long [`LocalObs`] waits for the connect.
pub const OBS_PROBE_TIMEOUT: Duration = Duration::from_millis(150);

/// The default probe: OBS WebSocket listening on this computer.
#[derive(Clone, Copy, Debug)]
pub struct LocalObs {
    /// The port to try.
    pub port: u16,
}

impl Default for LocalObs {
    fn default() -> Self {
        Self { port: OBS_PORT }
    }
}

impl ObsProbe for LocalObs {
    fn obs_running(&self) -> bool {
        let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, self.port));
        TcpStream::connect_timeout(&addr, OBS_PROBE_TIMEOUT).is_ok()
    }
}

/// Recommend the output to use on this machine: the virtual camera, or the best way into OBS when OBS runs.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{}]))]
pub struct Recommend {}

/// One output kind as the probe sees it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Candidate {
    /// The kind's name (`vcam`, `spout`, [`WINDOW`]).
    pub kind: String,
    /// `Err` with what a person can do when it can't run here.
    pub available: Result<(), String>,
}

/// A kind that can't run here, and what to do about it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Unavailable {
    /// The kind's name.
    pub kind: String,
    /// What a person can do.
    pub hint: String,
}

/// What `output.recommend` answers.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Recommendation {
    /// The kind to use, if any can run here: start it with `output.start` (or `output.window.open` for `window`).
    pub kind: Option<String>,
    /// The other kinds that can run here, best first.
    pub others: Vec<String>,
    /// Whether OBS is running.
    pub obs: bool,
    /// With OBS running, how to bring the pick into it.
    pub obs_hint: Option<String>,
    /// Kinds this build has that can't run here, and why.
    pub unavailable: Vec<Unavailable>,
}

/// The order kinds are preferred in: with OBS, the way into OBS that keeps the most (alpha, GPU) first.
fn rank(kind: &str, obs: bool, window: Option<WindowCaps>) -> u8 {
    let transparent = window.is_some_and(|c| c.transparent);
    let order: &[&str] = match (obs, transparent) {
        (true, true) => &["spout", "syphon", "ndi", WINDOW, "vcam"],
        (true, false) => &["spout", "syphon", "ndi", "vcam", WINDOW],
        (false, _) => &["vcam", WINDOW, "spout", "syphon", "ndi"],
    };
    // Spout and Syphon never meet (one per platform), so their order between them doesn't matter.
    let place = order.iter().position(|k| *k == kind).unwrap_or(order.len());
    u8::try_from(place).unwrap_or(u8::MAX)
}

/// How to bring `kind` into OBS.
fn obs_hint(kind: &str, window: Option<WindowCaps>) -> String {
    match kind {
        // OBS's Spout2 source shows an opaque picture until a composite mode is picked (spikes/broadcast-out).
        "spout" => "In OBS, add a Spout2 Capture source, choose Omuzaki and set Composite mode to Premultiplied Alpha: \
                    full quality, with transparency."
            .into(),
        "syphon" => "In OBS, add a Syphon Client source and choose Omuzaki: full quality, with transparency.".into(),
        "ndi" => "In OBS with the NDI plugin, add an NDI Source and choose Omuzaki.".into(),
        WINDOW if window.is_some_and(|c| c.transparent) => {
            "Open the output window (output.window.open), then add a Window Capture source of it in OBS; its background is transparent.".into()
        }
        WINDOW => "Open the output window (output.window.open), then add a Window Capture source of it in OBS with a Color Key filter on the key colour output.window.suggest_key gives.".into(),
        "vcam" => "In OBS, add a Video Capture Device source, choose the Omuzaki camera, and add a Color Key filter on its backdrop colour.".into(),
        other => format!("In OBS, add the source for {other} and choose Omuzaki."),
    }
}

/// The recommendation for `candidates`, the output window's capabilities (`None` without a window host) and whether
/// OBS runs. The window is a candidate whenever there is a host.
pub fn recommend(candidates: &[Candidate], window: Option<WindowCaps>, obs: bool) -> Recommendation {
    let mut usable = Vec::new();
    let mut unavailable = Vec::new();
    for candidate in candidates {
        match &candidate.available {
            Ok(()) => usable.push(candidate.kind.clone()),
            Err(hint) => unavailable.push(Unavailable { kind: candidate.kind.clone(), hint: hint.clone() }),
        }
    }
    if window.is_some() && !usable.iter().any(|k| k == WINDOW) {
        usable.push(WINDOW.into());
    }
    // A stable sort keeps the registry's order among equals.
    usable.sort_by_key(|kind| rank(kind, obs, window));
    let mut usable = usable.into_iter();
    let kind = usable.next();
    let obs_hint = kind.as_deref().filter(|_| obs).map(|k| obs_hint(k, window));
    Recommendation { kind, others: usable.collect(), obs, obs_hint, unavailable }
}

impl Op for Recommend {
    fn meta() -> OpMeta {
        OpMeta {
            name: "output.recommend",
            summary: "Recommend the output to use on this machine (the virtual camera, or the best way into OBS when OBS runs), with the others and why any can't run.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let candidates: Vec<Candidate> = output_kinds()
            .into_iter()
            .map(|k| Candidate { kind: k.name.into(), available: (k.available)().map_err(|e| e.to_string()) })
            .collect();
        let window = ctx.service::<dyn WindowHost>().ok().map(WindowHost::capabilities);
        let obs = match ctx.service::<dyn ObsProbe>() {
            Ok(probe) => probe.obs_running(),
            Err(_) => LocalObs::default().obs_running(),
        };
        let answer = recommend(&candidates, window, obs);
        let value = serde_json::to_value(&answer)
            .map_err(|e| OpError::internal(format!("serialising the answer failed: {e}")))?;
        Ok(Plan::answer(value))
    }
}

register_op!(Recommend);

#[cfg(test)]
mod tests {
    use super::*;

    fn kind(name: &str, ok: bool) -> Candidate {
        Candidate { kind: name.into(), available: if ok { Ok(()) } else { Err(format!("install {name}")) } }
    }

    const KEYED: Option<WindowCaps> = Some(WindowCaps::new(false, true));
    const CLEAR: Option<WindowCaps> = Some(WindowCaps::new(true, true));

    #[test]
    fn the_table_of_machines() {
        struct Row {
            kinds: Vec<Candidate>,
            window: Option<WindowCaps>,
            obs: bool,
            kind: Option<&'static str>,
            others: &'static [&'static str],
        }
        let rows = [
            // A virtual camera installed, no OBS: the camera, for every video app.
            Row { kinds: vec![kind("vcam", true)], window: KEYED, obs: false, kind: Some("vcam"), others: &["window"] },
            // OBS running, a keyed window and a camera: the camera beats a key in OBS.
            Row { kinds: vec![kind("vcam", true)], window: KEYED, obs: true, kind: Some("vcam"), others: &["window"] },
            // OBS running and a transparent window: the window keeps the alpha.
            Row { kinds: vec![kind("vcam", true)], window: CLEAR, obs: true, kind: Some("window"), others: &["vcam"] },
            // OBS running with Spout: Spout first, whatever else there is.
            Row {
                kinds: vec![kind("vcam", true), kind("spout", true)],
                window: CLEAR,
                obs: true,
                kind: Some("spout"),
                others: &["window", "vcam"],
            },
            // No camera installed, no OBS: the window.
            Row { kinds: vec![kind("vcam", false)], window: KEYED, obs: false, kind: Some("window"), others: &[] },
            // No camera installed, OBS running: the window, with OBS steps.
            Row { kinds: vec![kind("vcam", false)], window: KEYED, obs: true, kind: Some("window"), others: &[] },
            // Nothing at all (a headless build): no recommendation.
            Row { kinds: vec![kind("vcam", false)], window: None, obs: true, kind: None, others: &[] },
        ];
        for (n, row) in rows.into_iter().enumerate() {
            let got = recommend(&row.kinds, row.window, row.obs);
            assert_eq!(got.kind.as_deref(), row.kind, "row {n}: {got:?}");
            assert_eq!(got.others, row.others, "row {n}: {got:?}");
            assert_eq!(got.obs, row.obs, "row {n}");
            assert_eq!(got.obs_hint.is_some(), row.obs && row.kind.is_some(), "row {n}: {got:?}");
            let missing: Vec<_> = row.kinds.iter().filter(|k| k.available.is_err()).map(|k| k.kind.clone()).collect();
            assert_eq!(got.unavailable.iter().map(|u| u.kind.clone()).collect::<Vec<_>>(), missing, "row {n}");
        }
    }

    #[test]
    fn the_obs_steps_name_the_source_to_add() {
        let spout = recommend(&[kind("spout", true)], None, true);
        assert!(spout.obs_hint.is_some_and(|h| h.contains("Spout2 Capture") && h.contains("Premultiplied Alpha")));
        let keyed = recommend(&[], KEYED, true);
        assert!(keyed.obs_hint.is_some_and(|h| h.contains("Color Key") && h.contains("output.window.suggest_key")));
        let clear = recommend(&[], CLEAR, true);
        assert!(clear.obs_hint.is_some_and(|h| h.contains("transparent")));
        let missing = recommend(&[kind("vcam", false)], None, false);
        assert_eq!(missing.unavailable, [Unavailable { kind: "vcam".into(), hint: "install vcam".into() }]);
    }

    #[test]
    fn the_local_probe_sees_a_listener_and_its_absence() {
        let Ok(listener) = std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)) else { return };
        let Ok(port) = listener.local_addr().map(|a| a.port()) else { return };
        assert!(LocalObs { port }.obs_running());
        drop(listener);
        assert!(!LocalObs { port }.obs_running());
    }
}
