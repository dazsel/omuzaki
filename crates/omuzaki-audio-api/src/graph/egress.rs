//! Which captured audio reaches which output: the closure the privacy rule is built on (design doc section 7.2).
//!
//! **Audio never leaves the device unless a person routes it.** Every op that edits the graph computes
//! [`egress_pairs`] before and after; if the set grew and the actor is not a person, the op is denied. Making things
//! quieter or more private never grows the set.

use std::collections::BTreeSet;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::{AI_VOICE_BUS, BusSpec, DeviceRole, GraphSpec, MIC_BUS, MONITOR_OUT_ROLE, SourceRef};
use crate::semantics::is_silent_gain;

/// The state of the `mic` bus (design doc section 4.2). It is runtime state, not part of the project: a project saved
/// in `CohostOnly` reopens `Muted` ([`MicMode::restored`]).
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MicMode {
    /// The mic is on the stream and every voice tap hears it.
    #[default]
    Live,
    /// Nobody hears the mic, the AI included: every voice tap is silent and flagged muted.
    Muted,
    /// "Talk to co-host only": the stream mic is muted, only the AI's `listen` tap is open, and the AI's replies go to
    /// `monitor_out` only. Never the default. Only a person enters it; an agent may end it, and ending it lands on
    /// `Muted`.
    CohostOnly,
}

impl MicMode {
    /// The mode after leaving `CohostOnly`: `Muted`, never straight to `Live`, so ending a private talk cannot put the
    /// mic on stream by accident. Other modes are unchanged.
    #[must_use]
    pub const fn after_leaving_cohost(self) -> Self {
        match self {
            Self::CohostOnly => Self::Muted,
            other => other,
        }
    }

    /// The mode a reopened project starts in: `CohostOnly` is never restored, it comes back `Muted`.
    #[must_use]
    pub const fn restored(self) -> Self {
        self.after_leaving_cohost()
    }
}

/// What the egress closure needs to know about this machine and this moment.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Bindings {
    /// Input roles that are bound to a device: only these capture anything.
    pub inputs: BTreeSet<DeviceRole>,
    /// Output roles that are bound to a device: only these reach the outside.
    pub outputs: BTreeSet<DeviceRole>,
    /// The state of the `mic` bus.
    pub mic_mode: MicMode,
}

/// Audio captured from an input device, named by the role it is bound to.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
pub struct CapturedSource(pub DeviceRole);

/// Somewhere captured audio can leave to.
///
/// Network sinks and agents are not in the graph: a tap or an op that would hand them captured audio is gated where it
/// is requested (design doc section 7.2, rules 3 and 4).
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
#[non_exhaustive]
pub enum Egress {
    /// An output device, by role.
    Output {
        /// The role.
        role: DeviceRole,
    },
}

/// Where a walk through the graph has been, which decides what it may still reach.
#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct Visit {
    bus: usize,
    /// The signal has passed through the `ai_voice` bus.
    via_ai: bool,
}

/// The set of (captured source, egress) pairs the graph makes possible, given what is bound and the mic mode.
///
/// A pair is in the set when a path exists from an input role that is bound, through buses, to an output role that is
/// bound, along edges that are not cut. An edge is cut when:
/// - the bus it leaves is **muted**: that cuts every edge leaving it, pre-fader sends included;
/// - the bus gain is minus infinity (cuts post-fader sends and outputs), or the send's own gain is minus infinity;
/// - the bus is `mic` and the mode is `Muted` or `CohostOnly`: the mic reaches no output;
/// - the mode is `CohostOnly` and the path has passed through `ai_voice`: it may only end at `monitor_out`.
///
/// Taps are not edges: an input that only feeds a tap never egresses. Ducking does not cut an edge (its depth is
/// finite).
#[must_use]
pub fn egress_pairs(graph: &GraphSpec, bindings: &Bindings) -> BTreeSet<(CapturedSource, Egress)> {
    let mut pairs = BTreeSet::new();
    for (start, bus) in graph.buses.iter().enumerate() {
        for source in &bus.sources {
            let SourceRef::Device { role } = source else { continue };
            if !bindings.inputs.contains(role) {
                continue;
            }
            for output in reach(graph, bindings, start) {
                pairs.insert((CapturedSource(role.clone()), Egress::Output { role: output }));
            }
        }
    }
    pairs
}

/// The output roles reachable from the bus at `start`.
fn reach(graph: &GraphSpec, bindings: &Bindings, start: usize) -> BTreeSet<DeviceRole> {
    let mut outputs = BTreeSet::new();
    let mut seen = BTreeSet::new();
    let mut stack = vec![Visit { bus: start, via_ai: false }];
    while let Some(visit) = stack.pop() {
        if !seen.insert(visit) {
            continue;
        }
        let bus = &graph.buses[visit.bus];
        let via_ai = visit.via_ai || bus.id.as_str() == AI_VOICE_BUS;
        let cohost = bindings.mic_mode == MicMode::CohostOnly;
        if bus_is_cut(bus, bindings) {
            continue;
        }
        let post_fader_open = !is_silent_gain(bus.gain_db);
        for send in &bus.sends {
            let open = !is_silent_gain(send.gain_db) && (send.pre_fader || post_fader_open);
            if let (true, Some(to)) = (open, graph.buses.iter().position(|b| b.id == send.to)) {
                stack.push(Visit { bus: to, via_ai });
            }
        }
        if post_fader_open {
            let allowed = |role: &&DeviceRole| !(cohost && via_ai && role.as_str() != MONITOR_OUT_ROLE);
            outputs.extend(bus.outputs.iter().filter(|r| bindings.outputs.contains(*r)).filter(allowed).cloned());
        }
    }
    outputs
}

/// Whether nothing leaves `bus`: it is muted, or it is the mic while the mic is not live.
fn bus_is_cut(bus: &BusSpec, bindings: &Bindings) -> bool {
    bus.muted || (bus.id.as_str() == MIC_BUS && bindings.mic_mode != MicMode::Live)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graph::{BusId, Channels, SendSpec};

    fn role(s: &str) -> DeviceRole {
        DeviceRole::new(s).unwrap()
    }

    fn bus(id: &str, sources: &[&str], sends: &[&str], outputs: &[&str]) -> BusSpec {
        BusSpec {
            id: BusId::new(id).unwrap(),
            channels: Channels::Mono,
            sources: sources.iter().map(|r| SourceRef::Device { role: role(r) }).collect(),
            chain: vec![],
            gain_db: 0.0,
            muted: false,
            sends: sends
                .iter()
                .map(|t| SendSpec { to: BusId::new(*t).unwrap(), gain_db: 0.0, pre_fader: false })
                .collect(),
            outputs: outputs.iter().map(|r| role(r)).collect(),
        }
    }

    fn bindings(inputs: &[&str], outputs: &[&str], mic_mode: MicMode) -> Bindings {
        Bindings {
            inputs: inputs.iter().map(|r| role(r)).collect(),
            outputs: outputs.iter().map(|r| role(r)).collect(),
            mic_mode,
        }
    }

    fn pair(src: &str, out: &str) -> (CapturedSource, Egress) {
        (CapturedSource(role(src)), Egress::Output { role: role(out) })
    }

    /// `mic` -> `program` -> `program_out`, with a `monitor` bus fed by `ai_voice` and `mic`.
    fn base() -> GraphSpec {
        GraphSpec {
            buses: vec![
                bus("mic", &["voice"], &["program", "monitor"], &[]),
                bus("ai_voice", &[], &["program", "monitor"], &[]),
                bus("monitor", &[], &[], &["monitor_out"]),
                bus("program", &["game_in"], &[], &["program_out"]),
            ],
            ducks: vec![],
            ..GraphSpec::default()
        }
    }

    fn with(mut g: GraphSpec, edit: impl FnOnce(&mut GraphSpec)) -> GraphSpec {
        edit(&mut g);
        g
    }

    fn find<'a>(g: &'a mut GraphSpec, id: &str) -> &'a mut BusSpec {
        g.buses.iter_mut().find(|b| b.id.as_str() == id).unwrap()
    }

    #[test]
    #[allow(clippy::too_many_lines)] // a table of cases reads best in one place
    fn the_table() {
        let live = bindings(&["voice", "game_in"], &["program_out", "monitor_out"], MicMode::Live);
        let all_pairs = [pair("voice", "program_out"), pair("voice", "monitor_out"), pair("game_in", "program_out")]
            .into_iter()
            .collect::<BTreeSet<_>>();
        let cases: Vec<(&str, GraphSpec, Bindings, BTreeSet<_>)> = vec![
            ("everything open", base(), live.clone(), all_pairs.clone()),
            (
                "bus mute cuts the bus's edges",
                with(base(), |g| find(g, "mic").muted = true),
                live.clone(),
                [pair("game_in", "program_out")].into(),
            ),
            (
                "mute cuts pre-fader sends too",
                with(base(), |g| {
                    let m = find(g, "mic");
                    m.muted = true;
                    m.sends.iter_mut().for_each(|s| s.pre_fader = true);
                }),
                live.clone(),
                [pair("game_in", "program_out")].into(),
            ),
            (
                "gain at minus infinity cuts post-fader edges",
                with(base(), |g| find(g, "mic").gain_db = f32::NEG_INFINITY),
                live.clone(),
                [pair("game_in", "program_out")].into(),
            ),
            (
                "gain at minus infinity does not cut a pre-fader send",
                with(base(), |g| {
                    let m = find(g, "mic");
                    m.gain_db = f32::NEG_INFINITY;
                    m.sends.iter_mut().for_each(|s| s.pre_fader = true);
                }),
                live.clone(),
                all_pairs.clone(),
            ),
            (
                "a send at minus infinity cuts that send only",
                with(base(), |g| find(g, "mic").sends[0].gain_db = f32::NEG_INFINITY),
                live.clone(),
                [pair("voice", "monitor_out"), pair("game_in", "program_out")].into(),
            ),
            (
                "sends chain through buses",
                with(base(), |g| {
                    find(g, "mic").sends =
                        vec![SendSpec { to: BusId::new("monitor").unwrap(), gain_db: 0.0, pre_fader: false }];
                    find(g, "monitor").sends =
                        vec![SendSpec { to: BusId::new("program").unwrap(), gain_db: 0.0, pre_fader: false }];
                }),
                live.clone(),
                all_pairs.clone(),
            ),
            (
                "a muted bus in the middle of a chain cuts the rest",
                with(base(), |g| {
                    find(g, "mic").sends =
                        vec![SendSpec { to: BusId::new("monitor").unwrap(), gain_db: 0.0, pre_fader: false }];
                    find(g, "monitor").sends =
                        vec![SendSpec { to: BusId::new("program").unwrap(), gain_db: 0.0, pre_fader: false }];
                    find(g, "monitor").muted = true;
                }),
                live.clone(),
                [pair("game_in", "program_out")].into(),
            ),
            (
                "an input that only feeds a tap never egresses",
                with(base(), |g| find(g, "mic").sends.clear()),
                live.clone(),
                [pair("game_in", "program_out")].into(),
            ),
            (
                "an unbound output is not an egress",
                base(),
                bindings(&["voice", "game_in"], &["program_out"], MicMode::Live),
                [pair("voice", "program_out"), pair("game_in", "program_out")].into(),
            ),
            (
                "an unbound input captures nothing",
                base(),
                bindings(&["game_in"], &["program_out", "monitor_out"], MicMode::Live),
                [pair("game_in", "program_out")].into(),
            ),
            (
                "mic muted mode reaches no output",
                base(),
                bindings(&["voice", "game_in"], &["program_out", "monitor_out"], MicMode::Muted),
                [pair("game_in", "program_out")].into(),
            ),
            (
                "co-host only: the mic reaches no output",
                base(),
                bindings(&["voice", "game_in"], &["program_out", "monitor_out"], MicMode::CohostOnly),
                [pair("game_in", "program_out")].into(),
            ),
        ];
        for (name, graph, bound, want) in cases {
            assert_eq!(egress_pairs(&graph, &bound), want, "{name}");
        }
    }

    #[test]
    fn in_co_host_only_a_source_through_ai_voice_reaches_monitor_out_only() {
        // A line-in routed through the co-host's bus: live it reaches both outputs, in co-host mode only the monitor.
        let graph = with(base(), |g| find(g, "ai_voice").sources = vec![SourceRef::Device { role: role("line_in") }]);
        let live = bindings(&["line_in"], &["program_out", "monitor_out"], MicMode::Live);
        let private = Bindings { mic_mode: MicMode::CohostOnly, ..live.clone() };
        assert_eq!(
            egress_pairs(&graph, &live),
            [pair("line_in", "program_out"), pair("line_in", "monitor_out")].into()
        );
        assert_eq!(egress_pairs(&graph, &private), [pair("line_in", "monitor_out")].into());
    }

    #[test]
    fn making_things_more_private_never_grows_the_set() {
        let live = bindings(&["voice", "game_in"], &["program_out", "monitor_out"], MicMode::Live);
        let before = egress_pairs(&base(), &live);
        for mode in [MicMode::Muted, MicMode::CohostOnly] {
            assert!(egress_pairs(&base(), &Bindings { mic_mode: mode, ..live.clone() }).is_subset(&before));
        }
        let muted = with(base(), |g| find(g, "program").muted = true);
        assert!(egress_pairs(&muted, &live).is_subset(&before));
    }

    #[test]
    fn leaving_co_host_only_lands_on_muted() {
        assert_eq!(MicMode::CohostOnly.after_leaving_cohost(), MicMode::Muted);
        assert_eq!(MicMode::CohostOnly.restored(), MicMode::Muted);
        assert_eq!(MicMode::Live.restored(), MicMode::Live);
        assert_eq!(MicMode::Muted.after_leaving_cohost(), MicMode::Muted);
        assert_eq!(MicMode::default(), MicMode::Live);
    }
}
