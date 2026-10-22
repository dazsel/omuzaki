//! Keeps the live engine on the document's audio graph: the `audio.bus.*` ops edit `audio/graph`, and [`follow`]
//! applies it to the engine whenever the document changes (an edit, an undo, a redo, a rollback), from either side.
//!
//! Before any bus edit, and after undoing the first one, the document has no graph; the engine then runs the graph it
//! had when following began. A bound microphone always has its bus: if the document's graph has none, the mic bus is
//! added, as `audio.mic` does.

use std::sync::Arc;
use std::sync::mpsc::Sender;

use omuzaki_audio_api::engine::AudioEngine;
use omuzaki_audio_api::graph::GraphSpec;
use omuzaki_ops_api::{Grants, OpError, Runner, Scope};

use crate::bus_ops::graph_path;
use crate::ops::{ids, with_mic_bus};

/// Starts following `runner`'s document on a thread of its own, which ends when the runner goes away.
///
/// # Errors
/// What [`Runner::subscribe`] returns.
pub fn follow(engine: Arc<dyn AudioEngine>, runner: &Arc<dyn Runner>) -> Result<(), OpError> {
    follow_reporting(engine, runner, None)
}

/// [`follow`], sending each graph it applies to `applied`: the handshake a test waits on.
///
/// # Errors
/// What [`Runner::subscribe`] returns.
pub fn follow_reporting(
    engine: Arc<dyn AudioEngine>,
    runner: &Arc<dyn Runner>,
    applied: Option<Sender<GraphSpec>>,
) -> Result<(), OpError> {
    let grants = Grants::of([Scope::Read]);
    let events = runner.subscribe(&grants)?;
    let runner = Arc::downgrade(runner);
    let initial = engine.graph();
    let mut follower = Follower { engine, initial, applied: None, report: applied };
    if let Some(runner) = runner.upgrade() {
        follower.sync(&*runner, &grants);
    }
    std::thread::Builder::new()
        .name("audio graph follower".to_owned())
        .spawn(move || {
            while events.recv().is_ok() {
                let Some(runner) = runner.upgrade() else { break };
                follower.sync(&*runner, &grants);
            }
        })
        .map_err(|e| OpError::internal(format!("the audio graph follower did not start: {e}")))?;
    Ok(())
}

struct Follower {
    engine: Arc<dyn AudioEngine>,
    initial: GraphSpec,
    applied: Option<GraphSpec>,
    report: Option<Sender<GraphSpec>>,
}

impl Follower {
    /// Applies the document's graph if it differs from the one last applied.
    fn sync(&mut self, runner: &dyn Runner, grants: &Grants) {
        let stored = match runner.get(&graph_path(), grants) {
            Ok(Some(value)) => match serde_json::from_value::<GraphSpec>(value) {
                Ok(graph) => Some(graph),
                Err(error) => {
                    tracing::warn!(%error, "the project's audio graph is not in the graph format; keeping the last one");
                    return;
                }
            },
            Ok(None) => None,
            Err(error) => {
                tracing::warn!(error = %error.message, "the audio graph follower can't read the document");
                return;
            }
        };
        let mut graph = stored.unwrap_or_else(|| self.initial.clone());
        if let Ok((role, bus)) = ids()
            && self.engine.status().devices.iter().any(|(r, _)| r == &role)
            && let Some(with_mic) = with_mic_bus(&graph, &role, &bus)
        {
            graph = with_mic;
        }
        if self.applied.as_ref() == Some(&graph) {
            return;
        }
        match self.engine.apply(&graph) {
            Ok(()) => {
                if let Some(report) = &self.report {
                    // A test that stopped listening is no reason to stop following.
                    let _ = report.send(graph.clone());
                }
                self.applied = Some(graph);
            }
            Err(error) => tracing::warn!(%error, "the audio engine refused the project's audio graph"),
        }
    }
}
