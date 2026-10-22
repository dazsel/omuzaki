//! The worker's side of the protocol (`docs/design/supervisor.md` §4.2), for the worker binaries.
//!
//! [`WorkerConn::connect`] takes the control channel from stdin, says `Hello`, waits for `Start` and its shared
//! objects, and starts the **writer thread**: every 50 ms it sends one coalesced `Beat` with the progress counter's
//! value, and it sends announcements (`Frame`, `SlotFree`, `Opened`, `Ready`, `Gone`) as they are queued. Hot loops
//! only bump the counter ([`WorkerConn::beat`], one relaxed add) and queue announcements; they never touch the channel.
//!
//! A worker ends with its host: the channel's end (`recv` returns `None`) and, on Linux, the parent-death signal set
//! at connect, rechecked against the host's pid so a host that died before the signal was set is noticed too.

use std::time::Duration;

/// How often the writer thread sends a beat.
pub const BEAT_EVERY: Duration = Duration::from_millis(50);

/// The launcher's arguments, split from the worker's own.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WorkerArgs {
    /// The generation this process was started as.
    pub generation: u32,
    /// The host's process id.
    pub host_pid: u32,
    /// Every other argument, in order.
    pub rest: Vec<String>,
}

impl WorkerArgs {
    /// Splits `--generation <n>` and `--host-pid <pid>` from `args` (the program name already removed).
    ///
    /// # Errors
    /// A message when either is missing or not a number.
    pub fn parse(args: impl IntoIterator<Item = String>) -> Result<Self, String> {
        let mut out = Self::default();
        let (mut generation, mut host) = (None, None);
        let mut args = args.into_iter();
        while let Some(arg) = args.next() {
            match arg.as_str() {
                "--generation" | "--host-pid" => {
                    let value = args.next().ok_or_else(|| format!("{arg} needs a number"))?;
                    let n: u32 = value.parse().map_err(|_| format!("{arg}: {value} isn't a whole number"))?;
                    if arg == "--generation" {
                        generation = Some(n);
                    } else {
                        host = Some(n);
                    }
                }
                _ => out.rest.push(arg),
            }
        }
        out.generation = generation.ok_or("--generation is missing")?;
        out.host_pid = host.ok_or("--host-pid is missing")?;
        Ok(out)
    }
}

#[cfg(unix)]
pub use unix::{Started, WorkerConn};

#[cfg(unix)]
mod unix {
    use std::os::fd::{AsFd as _, OwnedFd};
    use std::os::unix::net::UnixStream;
    use std::sync::mpsc::{self, RecvTimeoutError, SyncSender};
    use std::thread::JoinHandle;
    use std::time::Instant;

    use omuzaki_stream_api::error::StreamError;
    use omuzaki_stream_api::supervisor::WorkerKind;
    use omuzaki_stream_api::worker::{ChannelNames, HostMsg, StartParams, WorkerMsg, read_msg_fds, write_msg};

    use super::{BEAT_EVERY, WorkerArgs};
    use crate::watchdog::policy::HeartbeatCell;

    /// The most announcements waiting for the writer: twice the most a worker has outstanding (its slots).
    const QUEUE: usize = 64;

    /// What `Start` gave this generation.
    #[derive(Debug)]
    pub struct Started {
        /// The objects' roles and references.
        pub channels: ChannelNames,
        /// The objects, in position order.
        pub fds: Vec<OwnedFd>,
        /// What the kind needs.
        pub params: StartParams,
    }

    impl Started {
        /// Takes the object the channels name for `role`, if any.
        pub fn take(&mut self, role: omuzaki_stream_api::worker::ChannelRole) -> Option<OwnedFd> {
            let position = match self.channels.get(role)? {
                omuzaki_stream_api::worker::ChannelRef::Fd { position, .. } => usize::from(*position),
                _ => return None,
            };
            let fd = self.fds.get(position)?.try_clone().ok()?;
            Some(fd)
        }
    }

    /// The worker's end of its control channel.
    #[derive(Debug)]
    pub struct WorkerConn {
        sock: UnixStream,
        beats: HeartbeatCell,
        queue: Option<SyncSender<WorkerMsg>>,
        writer: Option<JoinHandle<()>>,
    }

    fn io(e: &std::io::Error) -> StreamError {
        StreamError::Io(e.to_string())
    }

    impl WorkerConn {
        /// Connects over stdin as a worker of `kind`: `Hello`, then waits for `Start`, then starts the writer thread.
        ///
        /// # Errors
        /// [`StreamError`] when stdin is not the control channel, the host has gone, or the first message is not
        /// `Start`.
        pub fn connect(kind: WorkerKind, args: &WorkerArgs) -> Result<(Self, Started), StreamError> {
            die_with(args.host_pid);
            let stdin = std::io::stdin().as_fd().try_clone_to_owned().map_err(|e| io(&e))?;
            let sock = UnixStream::from(stdin);
            let mut writer = sock.try_clone().map_err(|e| io(&e))?;
            write_msg(&mut writer, &WorkerMsg::hello(kind, args.generation))?;
            let (msg, fds) = read_msg_fds::<HostMsg>(&sock, &mut Vec::new())?
                .ok_or_else(|| StreamError::Io("the host closed the channel before Start".into()))?;
            let HostMsg::Start { channels, params, .. } = msg else {
                return Err(StreamError::Protocol(format!("{msg:?} where Start was expected")));
            };
            let beats = HeartbeatCell::new();
            let (queue, rx) = mpsc::sync_channel::<WorkerMsg>(QUEUE);
            let cell = beats.clone();
            let writer = std::thread::Builder::new()
                .name("control-writer".into())
                .spawn(move || {
                    let mut next = Instant::now() + BEAT_EVERY;
                    loop {
                        let msg = match rx.recv_timeout(next.saturating_duration_since(Instant::now())) {
                            Ok(msg) => msg,
                            Err(RecvTimeoutError::Timeout) => {
                                next = Instant::now() + BEAT_EVERY;
                                WorkerMsg::beat(cell.read(), cell.work())
                            }
                            Err(RecvTimeoutError::Disconnected) => break,
                        };
                        if write_msg(&mut writer, &msg).is_err() {
                            // The host has gone.
                            break;
                        }
                    }
                })
                .map_err(|e| io(&e))?;
            let conn = Self { sock, beats, queue: Some(queue), writer: Some(writer) };
            Ok((conn, Started { channels, fds, params }))
        }

        /// One iteration of the main loop: a relaxed atomic add.
        pub fn beat(&self) {
            self.beats.beat();
        }

        /// Sets the work counter (frames, packets, blocks).
        pub fn set_work(&self, work: u64) {
            self.beats.set_work(work);
        }

        /// The progress and work counters, for a loop on another thread.
        pub fn heartbeat(&self) -> HeartbeatCell {
            self.beats.clone()
        }

        /// Queues `msg` for the writer thread; waits only when the queue is full.
        ///
        /// # Errors
        /// [`StreamError::Stopped`] when the writer has ended (the host has gone).
        pub fn send(&self, msg: WorkerMsg) -> Result<(), StreamError> {
            self.queue.as_ref().ok_or(StreamError::Stopped)?.send(msg).map_err(|_| StreamError::Stopped)
        }

        /// The next message from the host and its descriptors, reusing `buf`; `None` when the host has gone.
        ///
        /// # Errors
        /// [`StreamError::Protocol`] for a malformed frame.
        pub fn recv(&self, buf: &mut Vec<u8>) -> Result<Option<(HostMsg, Vec<OwnedFd>)>, StreamError> {
            match read_msg_fds::<HostMsg>(&self.sock, buf) {
                Ok(msg) => Ok(msg),
                Err(StreamError::Io(_)) => Ok(None),
                Err(e) => Err(e),
            }
        }

        /// Sends what is queued and ends the writer (a clean exit after `Stop`).
        pub fn close(mut self) {
            self.queue = None;
            if let Some(writer) = self.writer.take() {
                let _ = writer.join();
            }
        }
    }

    /// On Linux, asks to be killed when the host dies, then checks that the host is still the parent (it may have
    /// died before the request).
    fn die_with(host_pid: u32) {
        #[cfg(any(target_os = "linux", target_os = "android"))]
        {
            let _ = rustix::process::set_parent_process_death_signal(Some(rustix::process::Signal::KILL));
            let parent = rustix::process::getppid().map_or(0, |p| p.as_raw_nonzero().get().unsigned_abs());
            if parent != host_pid {
                std::process::exit(0);
            }
        }
        #[cfg(not(any(target_os = "linux", target_os = "android")))]
        let _ = host_pid;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_launchers_arguments_are_split_from_the_workers_own() {
        let args = ["--beat-ms", "20", "--generation", "3", "--host-pid", "77", "--x"].map(String::from);
        let parsed = WorkerArgs::parse(args).unwrap_or_default();
        assert_eq!(parsed.generation, 3);
        assert_eq!(parsed.host_pid, 77);
        assert_eq!(parsed.rest, ["--beat-ms", "20", "--x"].map(String::from));
        assert!(WorkerArgs::parse(["--generation".to_owned(), "1".to_owned()]).is_err());
    }
}
