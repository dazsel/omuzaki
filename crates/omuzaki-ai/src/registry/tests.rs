//! Unit tests for what the integration tests can't reach: the evaluation environment a smoke test gets.

use std::sync::Arc;

use omuzaki_ai_api::{EvalEnv, ModelRegistry, NoProgress, SlotRef};
use omuzaki_ai_mock::{ECHO_MODEL, MemFetcher};
use sha2::{Digest, Sha256};

use super::{CACHE_DIR, Registry, RegistrySettings};
use crate::VerifiedCache;

fn echo_slot() -> SlotRef {
    "test_echo@1".parse().unwrap()
}

/// `echo-fake` at `version`, whose file is the mock's echo model plus a comment naming the version (so each
/// version has its own bytes), requiring app `min_app`.
fn registration(version: &str, min_app: &str) -> ((String, String), (String, Vec<u8>)) {
    registration_of("echo-fake", "test_echo@1", version, min_app)
}

/// Like [`registration`], for any model id and slot.
fn registration_of(id: &str, slot: &str, version: &str, min_app: &str) -> ((String, String), (String, Vec<u8>)) {
    let bytes = format!("# {id} {version}\n{ECHO_MODEL}").into_bytes();
    let url = format!("https://models.invalid/{id}/{version}/echo.fake.toml");
    let hash = omuzaki_ai_api::Sha256(Sha256::digest(&bytes).into());
    let text = format!(
        "id = \"{id}\"\nversion = \"{version}\"\nname = \"Echo\"\nslot = \"{slot}\"\nruntime = \"fake\"\n\
         min_app_version = \"{min_app}\"\nproviders = [\"cpu\"]\n\n[[files]]\nname = \"echo.fake.toml\"\n\
         sha256 = \"{hash}\"\nsize = {}\nurls = [\"{url}\"]\n\n[card]\nregister = \"x\"\n\n\
         [card.licence]\nspdx = \"MIT OR Apache-2.0\"\n",
        bytes.len()
    );
    ((id.to_owned(), text), (url, bytes))
}

fn registry(dir: &std::path::Path, served: Vec<(String, Vec<u8>)>, models: Vec<(String, String)>) -> Registry {
    let net = served.into_iter().fold(MemFetcher::new(), |net, (url, bytes)| net.serving(url, bytes));
    let files = Arc::new(VerifiedCache::new(dir.join(CACHE_DIR), net));
    Registry::with_models(dir, files, "1.0.0".parse().unwrap(), RegistrySettings::default(), models).unwrap()
}

#[test]
fn the_shipped_default_an_evaluation_gets_is_the_one_open_falls_back_to() {
    let dir = tempfile::tempdir().unwrap();
    let ((id1, v1), file1) = registration("1.0.0", "0.1.0");
    let ((id2, v2), file2) = registration("2.0.0", "0.1.0");
    // A model of another slot with a higher version must not hide echo 1.0.0 from the fallback.
    let ((id3, v3), file3) = registration_of("stream-fake", "test_stream@1", "3.0.0", "0.1.0");
    let served = vec![file1.clone(), file2.clone(), file3];
    let others = vec![(id1.clone(), v1.clone()), (id3.clone(), v3.clone())];
    let first = registry(dir.path(), served.clone(), [others.clone(), vec![(id2, v2)]].concat());
    for entry in first.models(None) {
        first.install(&entry.model, &NoProgress).unwrap();
    }
    drop(first);

    // Reopened with 2.0.0 needing a newer app than this one: `open` falls back to 1.0.0.
    let ((id2, v2_too_new), _) = registration("2.0.0", "9.0.0");
    let reopened = registry(dir.path(), served, [others, vec![(id2, v2_too_new)]].concat());

    let opened = reopened.open(&echo_slot(), None).unwrap().info().model;
    let env = super::install::Env { registry: &reopened };
    let for_evaluation = env.shipped(&echo_slot()).unwrap().info().model;
    assert_eq!(opened.version, "1.0.0".parse().unwrap());
    assert_eq!(for_evaluation, opened);
}
