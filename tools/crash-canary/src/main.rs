//! Crash canary: measures whether a native-crash minidump carries camera-frame-like bytes.
//!
//! `crash-canary <dir>` runs the whole experiment: it re-runs itself as a victim in each (where, drop) case, lets the
//! SDK's crash reporter process write the dump, then searches every file under `<dir>` for the canary. Nothing is
//! sent: the transport below writes envelopes to files, and the DSN host is never contacted.

// The 1 MiB stack buffer is the thing being measured.
#![allow(clippy::large_stack_arrays)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use sentry::protocol::EnvelopeItem;
use sentry::{ClientOptions, Envelope, Transport, TransportFactory};
use sentry_minidump::MinidumpIntegration;

/// A 32-byte mark that cannot occur by chance; repeated to fill a 1 MiB buffer standing for a camera frame.
const MARK: &[u8; 32] = b"OMUZAKI-CANARY-NOT-A-CAMERA-FRAME!";
const FRAME_BYTES: usize = 1 << 20;

/// Writes each envelope the SDK would have sent to `<dir>/envelope-<n>.bin`, so no network is ever involved.
struct FileTransport(PathBuf);

impl Transport for FileTransport {
    fn send_envelope(&self, envelope: Envelope) {
        let mut bytes = Vec::new();
        if envelope.to_writer(&mut bytes).is_ok() {
            let n = std::fs::read_dir(&self.0).map_or(0, Iterator::count);
            let _ = std::fs::write(self.0.join(format!("envelope-{n}.bin")), bytes);
        }
    }
}

impl TransportFactory for FileTransport {
    fn create_transport(&self, _: &ClientOptions) -> Arc<dyn Transport> {
        Arc::new(FileTransport(self.0.clone()))
    }
}

/// The victim: hold the canary where `where_` says, start the SDK, then abort (SIGABRT is a native crash).
fn victim(dir: &Path, where_: &str, drop_dump: bool) {
    let mut integration = MinidumpIntegration::new().crashes_dir(dir);
    if drop_dump {
        // The only SDK hook that touches the dump is before_capture, on the scope; it can remove the attachment.
        integration = integration.before_capture(|scope, _| scope.clear_attachments());
    }
    let mut options = ClientOptions::default().add_integration(integration);
    options.dsn = "https://canary@127.0.0.1:9/1".parse().ok();
    options.transport = Some(Arc::new(FileTransport(dir.to_path_buf())));
    let _guard = sentry::init(options);

    let frame: Vec<u8> = MARK.iter().copied().cycle().take(FRAME_BYTES).collect();
    let stack_frame = if where_ == "stack" { stack_canary() } else { [0u8; FRAME_BYTES] };
    let heap_frame = if where_ == "heap" { frame } else { Vec::new() };
    std::hint::black_box((&stack_frame, &heap_frame));
    std::process::abort();
}

/// 1 MiB of the canary on the stack of the crashing thread (the main thread's stack is 8 MiB).
fn stack_canary() -> [u8; FRAME_BYTES] {
    let mut buf = [0u8; FRAME_BYTES];
    for (i, b) in buf.iter_mut().enumerate() {
        *b = MARK[i % MARK.len()];
    }
    buf
}

fn count(haystack: &[u8]) -> usize {
    haystack.windows(MARK.len()).filter(|w| *w == MARK).count()
}

/// Reports, per file under `dir`, its size and how many whole canary marks it holds, and whether the envelope has a
/// minidump attachment at all.
fn report(case: &str, dir: &Path) {
    let mut files: Vec<_> = std::fs::read_dir(dir).into_iter().flatten().flatten().map(|e| e.path()).collect();
    files.sort();
    for path in files {
        let Ok(bytes) = std::fs::read(&path) else { continue };
        let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
        let envelope_has_dump = Envelope::from_slice(&bytes)
            .map(|e| e.items().any(|i| matches!(i, EnvelopeItem::Attachment(a) if Path::new(&a.filename).extension().is_some_and(|x| x == "dmp"))));
        println!(
            "{case}\t{name}\t{} bytes\t{} marks\tdump-in-envelope={envelope_has_dump:?}",
            bytes.len(),
            count(&bytes)
        );
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.as_slice() {
        [_, dir, where_, drop] => victim(Path::new(dir), where_, drop == "drop"),
        [_, dir] => {
            let Ok(exe) = std::env::current_exe() else { return eprintln!("cannot find own path") };
            for (where_, drop) in [("heap", "keep"), ("stack", "keep"), ("none", "keep"), ("stack", "drop")] {
                let case = format!("{where_}-{drop}");
                let case_dir = Path::new(dir).join(&case);
                let _ = std::fs::create_dir_all(&case_dir);
                let status = Command::new(&exe).arg(&case_dir).arg(where_).arg(drop).status();
                println!("{case}: victim exited with {status:?}");
                report(&case, &case_dir);
            }
        }
        _ => eprintln!("usage: crash-canary <empty dir>"),
    }
}
