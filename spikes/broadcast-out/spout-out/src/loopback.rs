//! Runs the sender in this process and the receiver in a child process, the way OBS and an avatar app are two
//! programs, then puts the two reports side by side.

use std::io::Read;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use crate::receiver::ReceiveConfig;
use crate::report::{REPORT_PREFIX, ReceiverKind, ReceiverReport, SenderReport};
use crate::sender::{self, SendConfig, SendError};

/// How long to wait for the receiver to finish after the sender is done.
const CHILD_TIMEOUT: Duration = Duration::from_secs(30);

/// Why a loopback run failed.
#[derive(Debug, thiserror::Error)]
pub enum LoopbackError {
    /// The sender failed.
    #[error(transparent)]
    Send(#[from] SendError),
    /// The receiver process could not be started or did not finish.
    #[error("receiver process: {0}")]
    Child(String),
    /// The receiver printed no parsable report.
    #[error("receiver report missing or invalid: {0}")]
    Report(String),
}

/// Starts the receiver first (so it is already looking when the sender registers), runs the sender, then collects the
/// receiver's report from its stdout.
///
/// # Errors
/// Fails when either side fails; the receiver process is killed if it overruns.
pub fn run(
    send: &SendConfig,
    kind: ReceiverKind,
    poll_hz: f64,
) -> Result<(SenderReport, ReceiverReport), LoopbackError> {
    let exe = std::env::current_exe().map_err(|e| LoopbackError::Child(e.to_string()))?;
    let receive = ReceiveConfig { name: send.name.clone(), frames: send.frames, hash_every: send.hash_every };
    let mut child = Command::new(exe)
        .args(["receive", "--name", &receive.name, "--frames", &receive.frames.to_string()])
        .args(["--hash-every", &receive.hash_every.to_string()])
        .args(["--receiver", if kind == ReceiverKind::Raw { "raw" } else { "sdk" }])
        .args(["--poll-hz", &poll_hz.to_string()])
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .map_err(|e| LoopbackError::Child(e.to_string()))?;
    let mut stdout = child.stdout.take().ok_or_else(|| LoopbackError::Child("no stdout pipe".into()))?;
    let (tx, rx) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        let mut text = String::new();
        // A read error just leaves less text; the missing-report error below says what happened.
        let _ = stdout.read_to_string(&mut text);
        let _ = tx.send(text);
    });

    let sender_result = sender::run(send);
    let deadline = Instant::now() + CHILD_TIMEOUT;
    let finished = loop {
        match child.try_wait() {
            Ok(Some(_)) => break true,
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            _ => break false,
        }
    };
    if !finished {
        // By PID, never by pattern: the child is ours.
        let _ = child.kill();
        let _ = child.wait();
    }
    let sender_report = sender_result?;
    let text = rx.recv_timeout(Duration::from_secs(5)).unwrap_or_default();
    let _ = reader.join();
    if !finished {
        return Err(LoopbackError::Child(format!("did not finish within {CHILD_TIMEOUT:?} and was killed")));
    }
    let line = text
        .lines()
        .find_map(|l| l.strip_prefix(REPORT_PREFIX))
        .ok_or_else(|| LoopbackError::Report(format!("no {REPORT_PREFIX:?} line in {text:?}")))?;
    let receiver_report = serde_json::from_str(line).map_err(|e| LoopbackError::Report(e.to_string()))?;
    Ok((sender_report, receiver_report))
}
