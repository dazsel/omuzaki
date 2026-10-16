//! `omuzaki-model-worker`: the process the registry runs every model the person adds in (ADR 0016 rule 4, design
//! `docs/design/ai-api.md` section 9). It links the runtimes, so a runtime parsing an untrusted file can crash or stall
//! only this process, and serves `omuzaki_ai::sandbox` on stdin and stdout until the registry closes the pipe.
//!
//! It links no network stack: a runtime's native library is fetched by the registry, at the worker's request. It is
//! started by the registry, never by a person; run by hand it waits for requests on stdin.

use std::process::ExitCode;

fn main() -> ExitCode {
    // The runtimes register themselves; linking them is what makes them available here (ADR 0015).
    omuzaki_ai::link();
    omuzaki_ai_ort::link();
    omuzaki_ai_litert::link();
    omuzaki_ai::sandbox::serve()
}
