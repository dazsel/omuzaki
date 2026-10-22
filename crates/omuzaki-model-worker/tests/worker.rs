//! The real worker binary: it starts under the registry's sandbox, says it is ready, and reads models with the
//! runtimes it links. A file the runtime can't read is refused with the runtime's words, never a crash; a runtime it
//! doesn't link says so.
#![allow(clippy::unwrap_used)] // helpers outside `#[test]` fns: a panic is a clear test failure

use omuzaki_ai::VerifiedCache;
use omuzaki_ai::sandbox::{self, DEFAULT_MEMORY_MIB, Model, SandboxSettings};
use omuzaki_ai_api::{LoadError, RuntimeId};
use omuzaki_ai_mock::{MemFetcher, fake_model};

fn settings() -> SandboxSettings {
    SandboxSettings {
        program: env!("CARGO_BIN_EXE_omuzaki-model-worker").into(),
        args: Vec::new(),
        env: Vec::new(),
        memory_mib: DEFAULT_MEMORY_MIB,
    }
}

/// Probes a file of `bytes` as a model for `runtime` in the real worker.
fn probe(runtime: &str, bytes: &[u8]) -> Result<omuzaki_ai_api::Probe, LoadError> {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("model.bin");
    std::fs::write(&path, bytes).unwrap();
    let (model, runtime) = (fake_model(), RuntimeId::new(runtime).unwrap());
    let natives = VerifiedCache::new(dir.path().join("cache"), MemFetcher::new());
    let files = vec![("model.bin".to_owned(), path)];
    sandbox::probe(&settings(), &Model { model: &model, runtime: &runtime, files, stages: &[] }, &natives)
}

#[test]
fn the_worker_links_the_onnx_and_tflite_runtimes_and_refuses_what_they_cannot_read() {
    for runtime in ["onnx", "tflite"] {
        let error = probe(runtime, b"not a model").unwrap_err();
        let words = error.to_string();
        assert!(!words.contains("no `"), "{runtime} is linked: {words}");
        assert!(!words.contains("crashed"), "{runtime} refused it without crashing: {words}");
    }
}

#[test]
fn a_runtime_the_worker_does_not_link_is_named() {
    let error = probe("fake", b"scale = 1.0").unwrap_err();
    assert!(error.to_string().contains("no `fake` runtime"), "{error}");
}
