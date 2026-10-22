//! The fake worker: a small process the supervisor tests launch (`docs/design/supervisor.md` §14). It speaks the
//! worker protocol on its stdin like a real worker.
//!
//! - `--beat-ms N`: the main loop's period (default 33); each iteration is one beat;
//! - `--startup-ms N`: silence before the first iteration (default 0);
//! - `--crash-after-ms N`: exit with code 3 that long after starting;
//! - `--hang-after-ms N`: stop beating that long after starting, and stay alive;
//! - `--spam-stdout`: print megabytes without newlines, and garbage, to stdout (the host's log limits hold and the
//!   protocol is untouched);
//! - `--bad-frame`: send a malformed control frame after `Hello`;
//! - `--gone REASON`: say `Gone { reason }` after `Start` and exit (a configuration error);
//! - `--first-generation-only`: the crash and the hang apply to generation 1 only;
//! - `--kind KIND`: say `Hello` as another kind (renderer, tracker, audio, encoder, adapter; default renderer);
//! - with no role, its work counter counts the host's messages after `Start` (the latched state a generation was sent);
//! - `--role encoder`: an encoder over the `Frames` slots and the `Packets` channel (#1224): `Opened` with the
//!   `--extradata TEXT` bytes, one packet per announced frame (the slot's bytes, a keyframe first and on request,
//!   `BlockAdditional` side data), `SlotFree` after each, and a flush on `Stop`; with `--hang-after-ms` it stops
//!   beating and answering frames; `--extradata-per-generation` appends `-g<generation>` to the extradata.
//!
//! The launcher appends `--generation N --host-pid P`.

#[cfg(unix)]
fn main() {
    if let Err(error) = unix::run() {
        eprintln!("fake-worker: {error}");
        std::process::exit(2);
    }
}

#[cfg(not(unix))]
fn main() {
    eprintln!("fake-worker: the worker protocol needs Unix here");
    std::process::exit(2);
}

#[cfg(unix)]
mod unix {
    use std::io::Write as _;
    use std::time::{Duration, Instant};

    use bytes::Bytes;
    use omuzaki_shm::{FrameSlots, PacketChannel, Share};
    use omuzaki_stream::watchdog::worker::{Started, WorkerArgs, WorkerConn};
    use omuzaki_stream_api::encoder::{StreamSpec, VideoSpec};
    use omuzaki_stream_api::packet::{Packet, SideData, SideKind, StreamId};
    use omuzaki_stream_api::supervisor::WorkerKind;
    use omuzaki_stream_api::time::Timebase;
    use omuzaki_stream_api::worker::{ChannelRole, HostMsg, MAGIC, StartParams, WorkerMsg};

    /// The exit code of a scripted crash.
    const CRASH_CODE: i32 = 3;

    // Independent switches from the command line.
    #[allow(clippy::struct_excessive_bools)]
    #[derive(Debug, Default)]
    struct Script {
        beat: Duration,
        startup: Duration,
        crash_after: Option<Duration>,
        hang_after: Option<Duration>,
        spam: bool,
        bad_frame: bool,
        gone: Option<String>,
        kind: Option<WorkerKind>,
        encoder: bool,
        extradata: String,
        extradata_per_generation: bool,
        first_only: bool,
    }

    fn kind(name: &str) -> Result<WorkerKind, String> {
        Ok(match name {
            "renderer" => WorkerKind::Renderer,
            "tracker" => WorkerKind::Tracker,
            "audio" => WorkerKind::Audio,
            "encoder" => WorkerKind::Encoder,
            "adapter" => WorkerKind::Adapter,
            other => return Err(format!("unknown kind {other}")),
        })
    }

    fn script(rest: Vec<String>) -> Result<Script, String> {
        let mut script = Script { beat: Duration::from_millis(33), ..Script::default() };
        let mut args = rest.into_iter();
        while let Some(flag) = args.next() {
            match flag.as_str() {
                "--spam-stdout" => script.spam = true,
                "--bad-frame" => script.bad_frame = true,
                "--gone" => script.gone = Some(args.next().ok_or("--gone needs a reason")?),
                "--kind" => script.kind = Some(kind(&args.next().ok_or("--kind needs a kind")?)?),
                "--role" => match args.next().as_deref() {
                    Some("encoder") => script.encoder = true,
                    other => return Err(format!("unknown role {other:?}")),
                },
                "--extradata" => script.extradata = args.next().ok_or("--extradata needs text")?,
                "--extradata-per-generation" => script.extradata_per_generation = true,
                "--first-generation-only" => script.first_only = true,
                _ => {
                    let value = args.next().ok_or_else(|| format!("{flag} needs a number"))?;
                    let ms: u64 = value.parse().map_err(|_| format!("{flag}: {value} isn't a whole number"))?;
                    let n = Duration::from_millis(ms);
                    match flag.as_str() {
                        "--beat-ms" => script.beat = n.max(Duration::from_millis(1)),
                        "--startup-ms" => script.startup = n,
                        "--crash-after-ms" => script.crash_after = Some(n),
                        "--hang-after-ms" => script.hang_after = Some(n),
                        other => return Err(format!("unknown flag {other}")),
                    }
                }
            }
        }
        Ok(script)
    }

    pub(super) fn run() -> Result<(), String> {
        let args = WorkerArgs::parse(std::env::args().skip(1))?;
        let mut script = script(args.rest.clone())?;
        if script.first_only && args.generation > 1 {
            (script.crash_after, script.hang_after) = (None, None);
        }
        let kind = script.kind.unwrap_or(if script.encoder { WorkerKind::Encoder } else { WorkerKind::Renderer });
        let start = Instant::now();
        if script.bad_frame {
            // Hello, then a frame with a bad magic straight onto the channel.
            return bad_frame(kind, &args);
        }
        let (conn, started) = WorkerConn::connect(kind, &args).map_err(|e| e.to_string())?;
        if let Some(reason) = &script.gone {
            conn.send(WorkerMsg::gone(reason.clone())).map_err(|e| e.to_string())?;
            conn.close();
            return Ok(());
        }
        if script.spam {
            spam();
        }
        if script.encoder {
            return encoder(&script, conn, started, start, args.generation);
        }
        std::thread::sleep(script.startup);
        let conn = std::sync::Arc::new(conn);
        // The host's messages: a `Stop` ends the worker cleanly; the channel's end ends it too. The work counter
        // counts the others, so a test sees the latched state replayed to each generation.
        let reader = std::sync::Arc::clone(&conn);
        std::thread::spawn(move || {
            let mut buf = Vec::new();
            let mut seen = 0_u64;
            loop {
                match reader.recv(&mut buf) {
                    Ok(Some((HostMsg::Stop, _)) | None) | Err(_) => std::process::exit(0),
                    Ok(Some(_)) => {
                        seen += 1;
                        reader.set_work(seen);
                    }
                }
            }
        });
        let after = |at: Option<Duration>| at.is_some_and(|at| start.elapsed() >= at);
        loop {
            if after(script.crash_after) {
                std::process::exit(CRASH_CODE);
            }
            if !after(script.hang_after) {
                conn.beat();
            }
            std::thread::sleep(script.beat);
        }
    }

    fn bad_frame(kind: WorkerKind, args: &WorkerArgs) -> Result<(), String> {
        use std::os::fd::AsFd as _;
        let stdin = std::io::stdin().as_fd().try_clone_to_owned().map_err(|e| e.to_string())?;
        let mut sock = std::os::unix::net::UnixStream::from(stdin);
        omuzaki_stream_api::worker::write_msg(&mut sock, &WorkerMsg::hello(kind, args.generation))
            .map_err(|e| e.to_string())?;
        let mut junk = b"JUNK".to_vec();
        junk.extend_from_slice(&MAGIC);
        sock.write_all(&junk).map_err(|e| e.to_string())?;
        // Stay alive: the host must kill it.
        std::thread::sleep(Duration::from_secs(30));
        Ok(())
    }

    /// The `Opened` extradata: the text, with the generation appended when it changes with each.
    fn extradata(script: &Script, generation: u32) -> String {
        if script.extradata_per_generation {
            format!("{}-g{generation}", script.extradata)
        } else {
            script.extradata.clone()
        }
    }

    /// Megabytes without a newline, then garbage bytes, on stdout.
    fn spam() {
        let mut out = std::io::stdout().lock();
        let block = vec![b'x'; 64 * 1024];
        for _ in 0..64 {
            let _ = out.write_all(&block);
        }
        let _ = out.write_all(&[0xFF, 0x00, 0xFE, b'\n', 0x80, b'\n']);
        let _ = out.flush();
    }

    fn encoder(
        script: &Script,
        conn: WorkerConn,
        mut started: Started,
        start: Instant,
        generation: u32,
    ) -> Result<(), String> {
        let StartParams::Encoder { config, first_index, .. } = started.params.clone() else {
            return Err("an encoder needs encoder parameters".into());
        };
        let slots = started.take(ChannelRole::Frames).ok_or("no Frames slots")?;
        let packets = started.take(ChannelRole::Packets).ok_or("no Packets channel")?;
        let slots = FrameSlots::open(Share::Fd(slots)).map_err(|e| e.to_string())?;
        let packets = PacketChannel::open(Share::Fd(packets)).map_err(|e| e.to_string())?;
        let video = VideoSpec { size: config.video.size, fps: config.video.fps, alpha: config.video.alpha };
        let spec = StreamSpec::video(config.codec, "video", Timebase { num: video.fps.1, den: video.fps.0 }, video)
            .map_err(|e| e.to_string())?
            .with_extradata(Bytes::from(extradata(script, generation).into_bytes()));
        conn.send(WorkerMsg::opened(spec)).map_err(|e| e.to_string())?;
        conn.send(WorkerMsg::ready(0.0, 0.0, 0.0)).map_err(|e| e.to_string())?;
        let mut keyframe = true;
        let mut sent = 0_u64;
        let mut buf = Vec::new();
        let _ = first_index;
        let hung = move |at: Option<Duration>| at.is_some_and(|at| start.elapsed() >= at);
        // The main loop's beat, idle or not, until the scripted hang.
        let cell = conn.heartbeat();
        let hang_after = script.hang_after;
        std::thread::spawn(move || {
            while !hung(hang_after) {
                cell.beat();
                std::thread::sleep(Duration::from_millis(20));
            }
        });
        loop {
            if script.crash_after.is_some_and(|at| start.elapsed() >= at) {
                std::process::exit(CRASH_CODE);
            }
            let Some((msg, _)) = conn.recv(&mut buf).map_err(|e| e.to_string())? else { return Ok(()) };
            match msg {
                HostMsg::Keyframe => keyframe = true,
                HostMsg::Frame { .. } if hung(script.hang_after) => {
                    // Stops answering, and stays alive: the host must find it hung and kill it.
                    std::thread::sleep(Duration::from_secs(30));
                }
                HostMsg::Frame { slot, index, pts, .. } => {
                    let bytes = slots.read(slot).map_err(|e| e.to_string())?;
                    let index_bytes = index.to_le_bytes();
                    let packet = Packet {
                        stream: StreamId(0),
                        pts,
                        dts: pts,
                        duration: 1,
                        keyframe,
                        data: Bytes::from(bytes),
                        side_data: vec![SideData {
                            kind: SideKind::BlockAdditional { id: 1 },
                            data: Bytes::copy_from_slice(&index_bytes),
                        }],
                    };
                    keyframe = false;
                    while !packets.send(&packet).map_err(|e| e.to_string())? {
                        std::thread::sleep(Duration::from_millis(1));
                    }
                    sent += 1;
                    conn.set_work(sent);
                    conn.send(WorkerMsg::slot_free(slot)).map_err(|e| e.to_string())?;
                }
                HostMsg::Stop => {
                    conn.close();
                    return Ok(());
                }
                _ => {}
            }
        }
    }
}
