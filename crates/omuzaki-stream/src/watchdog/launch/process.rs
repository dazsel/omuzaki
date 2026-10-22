//! Workers as processes: a command per worker; the child writes one byte to stdout per beat, which a reader thread
//! counts into a [`HeartbeatCell`].

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::Read;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};

use super::{ExitInfo, LaunchError, WorkerHandle, WorkerLauncher};
use crate::watchdog::policy::{HeartbeatCell, WorkerId};

/// A program and its arguments.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkerCommand {
    /// The program.
    pub program: PathBuf,
    /// Its arguments.
    pub args: Vec<OsString>,
}

impl WorkerCommand {
    /// `program` with `args`.
    pub fn new(program: impl Into<PathBuf>, args: impl IntoIterator<Item = impl Into<OsString>>) -> Self {
        Self { program: program.into(), args: args.into_iter().map(Into::into).collect() }
    }
}

/// Launches each worker from its command.
#[derive(Clone, Debug, Default)]
pub struct ProcessLauncher {
    commands: BTreeMap<WorkerId, WorkerCommand>,
}

impl ProcessLauncher {
    /// No commands yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// With worker `id` launched by `command`.
    #[must_use]
    pub fn with(mut self, id: WorkerId, command: WorkerCommand) -> Self {
        self.commands.insert(id, command);
        self
    }
}

impl WorkerLauncher for ProcessLauncher {
    fn spawn(&self, id: WorkerId) -> Result<Box<dyn WorkerHandle>, LaunchError> {
        let command = self.commands.get(&id).ok_or(LaunchError::Unknown(id))?;
        let mut child = Command::new(&command.program)
            .args(&command.args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let cell = HeartbeatCell::new();
        if let Some(mut stdout) = child.stdout.take() {
            let beats = cell.clone();
            // Ends when the pipe closes: the child exited or was killed.
            std::thread::Builder::new().name(format!("heartbeat-{}", id.0)).spawn(move || {
                let mut buf = [0_u8; 64];
                while let Ok(n @ 1..) = stdout.read(&mut buf) {
                    (0..n).for_each(|_| beats.beat());
                }
            })?;
        }
        Ok(Box::new(ProcessHandle { child, cell, exited: None }))
    }
}

/// A running child.
#[derive(Debug)]
struct ProcessHandle {
    child: Child,
    cell: HeartbeatCell,
    exited: Option<ExitInfo>,
}

impl WorkerHandle for ProcessHandle {
    fn heartbeat(&self) -> u64 {
        self.cell.read()
    }

    fn try_wait(&mut self) -> Option<ExitInfo> {
        if self.exited.is_none()
            && let Ok(Some(status)) = self.child.try_wait()
        {
            self.exited = Some(ExitInfo { code: status.code() });
        }
        self.exited
    }

    fn kill(&mut self) {
        if self.exited.is_none() {
            // An error means it has already gone, which is what was asked.
            let _ = self.child.kill();
        }
    }

    fn pid(&self) -> Option<u32> {
        Some(self.child.id())
    }
}

impl Drop for ProcessHandle {
    fn drop(&mut self) {
        // Never leave a zombie: kill a live child and reap it (quick once killed).
        if self.exited.is_none() {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}
