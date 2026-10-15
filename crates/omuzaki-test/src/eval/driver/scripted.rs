//! The scripted driver (design 6.3): replays a script against a host, exactly as an agent's calls would arrive, checks
//! the `expect` and `assert` lines, and records what the server answered. No model, no network, deterministic.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use serde_json::{Map, Value};

use super::{Attempt, Driver, Stop};
use crate::eval::check::docglob::{differences, join};
use crate::eval::host::{Host, HostError, Reply, within};
use crate::eval::placeholder::fill;
use crate::eval::script::{AssertStep, CallStep, START, Script, Step, is_subset};
use crate::eval::task::{Caps, Task};
use crate::eval::transcript::{Event, EventLog};

/// How many paths a "the document differs" message names.
const SHOWN: usize = 3;

/// Replays a [`Script`].
#[derive(Debug, Clone)]
pub struct Scripted {
    script: Script,
    exports: BTreeMap<String, String>,
}

/// A call and the document after it, kept under a `save` name.
struct Saved {
    tool: String,
    args: Map<String, Value>,
    structured: Option<Value>,
    document: Value,
}

impl Scripted {
    /// A driver for `script`, whose `{name}` references the fixture's `exports` (already resolved to ids) fill in.
    #[must_use]
    pub fn new(script: Script, exports: BTreeMap<String, String>) -> Self {
        Self { script, exports }
    }
}

/// One attempt in progress.
struct Run<'a> {
    host: &'a Host,
    started: Instant,
    wall: Duration,
    log: EventLog,
    calls: u64,
    saved: BTreeMap<String, Saved>,
}

impl Run<'_> {
    fn t_ms(&self) -> u64 {
        u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    /// One call to the server, recorded as the model would have made it.
    fn call(&mut self, tool: &str, args: Map<String, Value>) -> Result<Reply, HostError> {
        let t = self.t_ms();
        self.log.push(t, Event::ToolCall { tool: tool.to_owned(), args: Value::Object(args.clone()) });
        self.calls += 1;
        let reply = self.host.call(tool, args, within(self.started, self.wall))?;
        let t = self.t_ms();
        self.log.push(
            t,
            Event::ToolResult { tool: tool.to_owned(), is_error: reply.is_error, structured: reply.structured.clone() },
        );
        Ok(reply)
    }

    fn document_matches(&self, name: &str) -> Result<Result<(), String>, HostError> {
        let now = self.host.document()?;
        let then = if name == START {
            self.host.start_document()
        } else {
            self.saved
                .get(name)
                .map(|s| &s.document)
                .ok_or_else(|| HostError::Document(format!("nothing saved as `{name}`")))?
        };
        if &now == then {
            return Ok(Ok(()));
        }
        let paths = differences(then, &now);
        let shown: Vec<String> = paths.iter().take(SHOWN).map(|p| format!("`{}`", join(p))).collect();
        let more = paths.len().saturating_sub(SHOWN);
        let tail = if more > 0 { format!(" and {more} more") } else { String::new() };
        Ok(Err(format!("the document differs from `{name}` at {}{tail}", shown.join(", "))))
    }
}

impl Driver for Scripted {
    fn run(&self, task: &Task, host: &Host, caps: &Caps) -> Result<Attempt, HostError> {
        let mut run = Run {
            host,
            started: Instant::now(),
            wall: Duration::from_secs(caps.wall_s),
            log: EventLog::new(),
            calls: 0,
            saved: BTreeMap::new(),
        };
        run.log.push(0, Event::Note { text: "task".to_owned(), data: Some(Value::String(task.id.clone())) });
        run.log.push(
            0,
            Event::Note {
                text: "exports".to_owned(),
                data: Some(serde_json::to_value(&self.exports).unwrap_or_default()),
            },
        );
        let mut stop = Stop::Finished;
        for line in &self.script.lines {
            if run.started.elapsed() >= run.wall {
                stop = Stop::WallClock;
                break;
            }
            let outcome = match &line.step {
                Step::Call(call) => self.call_step(&mut run, call, caps),
                Step::Assert(assert) => assert_step(&mut run, assert, caps),
            };
            match outcome {
                Ok(None) => {}
                Ok(Some(reason)) => {
                    stop = match reason {
                        Halt::Failed(reason) => Stop::ScriptFailed { line: line.number, reason },
                        Halt::ToolCallCap => Stop::ToolCallCap,
                    };
                    break;
                }
                Err(HostError::Timeout(_)) => {
                    stop = Stop::WallClock;
                    break;
                }
                Err(e) => return Err(e),
            }
        }
        let wall_ms = run.t_ms();
        Ok(Attempt { log: run.log, stop, tool_calls: run.calls, wall_ms })
    }
}

/// Why a step ended the attempt.
enum Halt {
    Failed(String),
    ToolCallCap,
}

impl Scripted {
    fn call_step(&self, run: &mut Run<'_>, step: &CallStep, caps: &Caps) -> Result<Option<Halt>, HostError> {
        if run.calls >= caps.tool_calls {
            return Ok(Some(Halt::ToolCallCap));
        }
        let args = match fill(&Value::Object(step.args.clone()), &self.exports) {
            Ok(Value::Object(args)) => args,
            Ok(_) => return Ok(Some(Halt::Failed("the arguments are not an object".to_owned()))),
            Err(reason) => return Ok(Some(Halt::Failed(reason))),
        };
        let reply = run.call(&step.call, args.clone())?;
        if let Some(expect) = &step.expect {
            let filled = match fill(expect, &self.exports) {
                Ok(filled) => filled,
                Err(reason) => return Ok(Some(Halt::Failed(reason))),
            };
            let holds = reply.structured.as_ref().is_some_and(|actual| is_subset(&filled, actual));
            if !holds {
                let got = reply.structured.as_ref().map_or_else(|| reply.text.clone(), Value::to_string);
                return Ok(Some(Halt::Failed(format!("`{}` answered {got}, expected {filled}", step.call))));
            }
        }
        if let Some(name) = &step.save {
            let document = run.host.document()?;
            run.saved
                .insert(name.clone(), Saved { tool: step.call.clone(), args, structured: reply.structured, document });
        }
        Ok(None)
    }
}

fn assert_step(run: &mut Run<'_>, step: &AssertStep, caps: &Caps) -> Result<Option<Halt>, HostError> {
    if let Some(name) = &step.doc_same_as
        && let Err(reason) = run.document_matches(name)?
    {
        return Ok(Some(Halt::Failed(reason)));
    }
    if let Some(name) = &step.render_same_as {
        if run.calls >= caps.tool_calls {
            return Ok(Some(Halt::ToolCallCap));
        }
        let Some(saved) = run.saved.get(name) else {
            return Ok(Some(Halt::Failed(format!("nothing saved as `{name}`"))));
        };
        let (tool, args, before) = (saved.tool.clone(), saved.args.clone(), saved.structured.clone());
        let again = run.call(&tool, args)?;
        if again.structured != before {
            return Ok(Some(Halt::Failed(format!(
                "running `{tool}` again gave a different answer than when `{name}` was saved"
            ))));
        }
    }
    Ok(None)
}
