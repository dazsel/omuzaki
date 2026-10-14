//! The ONNX Runtime flavour is chosen once per process, by the app's preference, even when a probe (which wants no
//! provider) comes first. Process-global state, so this is the only test in its file. Needs `OMUZAKI_ORT_CACHE`.
#![allow(clippy::unwrap_used)] // a panic is a test failure

mod common;

use std::path::PathBuf;
use std::sync::Mutex;

use common::{DirSource, cache, fixture, local, with_source};
use omuzaki_ai_api::{FetchError, FileSource, PinnedFile, Progress, Provider, Runtime as _};
use omuzaki_ai_ort::{OrtRuntime, builds, loaded, prefer_providers};

/// Remembers which archives were asked for, in order.
struct Recording {
    inner: DirSource,
    asked: Mutex<Vec<String>>,
}

impl FileSource for Recording {
    fn get(&self, pin: &PinnedFile, progress: &dyn Progress) -> Result<PathBuf, FetchError> {
        self.asked.lock().unwrap().push(pin.urls[0].rsplit('/').next().unwrap().to_owned());
        self.inner.get(pin, progress)
    }
}

#[test]
fn a_probe_first_does_not_fix_the_cpu_build_when_the_person_opted_in_to_cuda() {
    let Some(dir) = cache("a_probe_first_does_not_fix_the_cpu_build_when_the_person_opted_in_to_cuda") else { return };
    let cuda = builds::select_here(&[Provider::Cuda]).filter(|b| b.has(Provider::Cuda));
    let default = builds::select_here(&[]);
    let (Some(cuda), Some(default)) = (cuda, default) else {
        eprintln!("skipping: no CUDA build is pinned for this platform");
        return;
    };
    let natives = Recording { inner: dir, asked: Mutex::new(Vec::new()) };

    assert!(prefer_providers(&[Provider::Cuda, Provider::Cpu]), "nothing is loaded yet");
    let path = fixture("identity.onnx");
    let files = [local("model.onnx", &path)];
    with_source(&files, &[], &natives, |source| OrtRuntime.probe(source)).unwrap();

    // The probe asked for the CUDA archive first (the test cache lacks it, so the default build served instead).
    let asked = natives.asked.lock().unwrap().clone();
    assert_eq!(asked, [cuda.asset, default.asset], "the preference chooses the flavour even for a probe");
    assert_eq!(loaded().unwrap().build.id, default.id);
    assert!(!prefer_providers(&[Provider::Cpu]), "too late: the library is loaded for the life of the process");
}
