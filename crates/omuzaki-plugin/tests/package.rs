//! Packages (#1331, #1569; design §7 "Package", §3.2, §6.1): hostile archives, the content hash and its golden
//! vector, signatures, staging, installs keeping two versions, approval keys and dev keys, and the code cache.

// Integration-test helpers may panic: a panic is a clear test failure (clippy.toml covers only #[test] fns).
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::fmt::Write as _;
use std::io::Write as _;
use std::path::Path;
use std::sync::Arc;

use ed25519_dalek::{Signer as _, SigningKey};
use omuzaki_identity_api::domain::{Domain, signing_message};
use omuzaki_identity_api::key::{KeyId, Signature};
use omuzaki_ops_api::Grants;
use omuzaki_plugin::cache::{CacheOutcome, CodeCache};
use omuzaki_plugin::install::{ApprovalKey, InstallRoot, MachineId, Signer};
use omuzaki_plugin::package::{
    PackageError, PackageLimits, PackageSignature, PluginPackage, PublisherCheck, content_hash, hash_folder,
};
use omuzaki_plugin::{Storage, WasmPlugins};
use omuzaki_plugin_api::state::PluginState;
use omuzaki_plugin_api::{PluginError, Plugins};
use omuzaki_plugin_mock::conformance::{World, person};
use omuzaki_store_api::ContentHash;
use zip::write::SimpleFileOptions;

/// The digest of `tests/fixtures/golden-package` (a nested path and a non-ASCII name among its files); it must be
/// the same on Linux, macOS and Windows.
const GOLDEN: &str = "398747ff2d87195b9819ba488f4137616209e40c91efeae09c7f59b61f30c18f";

/// A test key; never a real one.
fn key() -> SigningKey {
    SigningKey::from_bytes(&[0x42; 32])
}

fn manifest_text(signed: bool, extra: &str) -> String {
    let mut text = String::from(
        "id = \"example\"\nversion = \"0.1.0\"\napi = \"^0.1\"\nlicence = \"MIT\"\nname = \"Example\"\ncomponent = \"plugin.wasm\"\n",
    );
    if signed {
        let fingerprint = KeyId(key().verifying_key().to_bytes()).fingerprint();
        writeln!(text, "publisher = {{ name = \"Example\", key = \"ed25519:{fingerprint}\" }}").unwrap();
    }
    text + extra
}

/// The files of a package: its manifest, its component and a translation.
fn files(signed: bool, component: &[u8]) -> Vec<(String, Vec<u8>)> {
    vec![
        ("plugin.toml".into(), manifest_text(signed, "").into_bytes()),
        ("plugin.wasm".into(), component.to_vec()),
        ("l10n/en/example.ftl".into(), b"plugin-example-name = Example".to_vec()),
    ]
}

/// `plugin.sig` for `files`, signed by the test key.
fn signature_of(files: &[(String, Vec<u8>)]) -> Vec<u8> {
    let hash = content_hash(files.iter().map(|(p, b)| (p.as_str(), b.as_slice())));
    let sig = key().sign(&signing_message(Domain::Plugin, &hash));
    let envelope =
        PackageSignature { key: KeyId(key().verifying_key().to_bytes()), signature: Signature(sig.to_bytes()) };
    serde_json::to_vec(&envelope).unwrap()
}

/// A `.zip` of `entries` in that order.
fn zip_of(entries: &[(String, Vec<u8>)]) -> Vec<u8> {
    let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    for (name, bytes) in entries {
        writer.start_file(name.as_str(), SimpleFileOptions::default()).unwrap();
        writer.write_all(bytes).unwrap();
    }
    writer.finish().unwrap().into_inner()
}

fn signed_zip(component: &[u8], reverse: bool) -> Vec<u8> {
    let mut entries = files(true, component);
    let sig = signature_of(&entries);
    entries.push(("plugin.sig".into(), sig));
    if reverse {
        entries.reverse();
    }
    zip_of(&entries)
}

fn open(bytes: &[u8]) -> Result<PluginPackage, PackageError> {
    PluginPackage::open_zip(bytes, &PackageLimits::default())
}

#[test]
fn package_hash_golden() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/golden-package");
    let hash = hash_folder(&root, &PackageLimits::default()).unwrap();
    // The preimage, written out again here: sorted paths, each with its u64 LE length, then the content's.
    let mut preimage = Vec::new();
    for (path, bytes) in [
        ("a.txt", b"alpha".as_slice()),
        ("l10n/ja/übersicht.ftl", b"gamma".as_slice()),
        ("nested/deeper/b.txt", b"beta".as_slice()),
    ] {
        preimage.extend_from_slice(&(path.len() as u64).to_le_bytes());
        preimage.extend_from_slice(path.as_bytes());
        preimage.extend_from_slice(&(bytes.len() as u64).to_le_bytes());
        preimage.extend_from_slice(bytes);
    }
    assert_eq!(hash, ContentHash(*blake3::hash(&preimage).as_bytes()), "the preimage is the design's");
    assert_eq!(hash.to_string(), GOLDEN);
}

#[test]
fn a_signed_package_verifies_and_its_hash_ignores_the_zip_order() {
    let component = common::ok_component();
    let package = open(&signed_zip(&component, false)).unwrap();
    assert_eq!(package.publisher(), PublisherCheck::Verified(KeyId(key().verifying_key().to_bytes())));
    assert_eq!(open(&signed_zip(&component, true)).unwrap().hash, package.hash);
    assert_eq!(package.component(), component.as_slice());

    // One byte changed in the component after signing: the hash moves and the signature no longer holds.
    let mut entries = files(true, &component);
    let sig = signature_of(&entries);
    entries[1].1[10] ^= 1;
    entries.push(("plugin.sig".into(), sig));
    let tampered = open(&zip_of(&entries)).unwrap();
    assert_ne!(tampered.hash, package.hash);
    assert_eq!(tampered.publisher(), PublisherCheck::Mismatch);
}

#[test]
fn unsigned_packages_open_and_a_claimed_publisher_without_a_signature_does_not_verify() {
    let component = common::ok_component();
    assert_eq!(open(&zip_of(&files(false, &component))).unwrap().publisher(), PublisherCheck::Unsigned);
    assert_eq!(open(&zip_of(&files(true, &component))).unwrap().publisher(), PublisherCheck::Mismatch);
}

#[test]
fn every_hostile_archive_is_refused_with_its_own_error() {
    let component = common::ok_component();
    let with = |name: &str| {
        let mut entries = files(false, &component);
        entries.push((name.into(), b"x".to_vec()));
        open(&zip_of(&entries)).unwrap_err()
    };
    assert!(matches!(with("../x"), PackageError::UnsafePath { why, .. } if why.contains("..")));
    assert!(matches!(with("a/../../x"), PackageError::UnsafePath { .. }));
    assert!(matches!(with("/etc/x"), PackageError::UnsafePath { why: "is absolute", .. }));
    assert!(matches!(with("C:/x"), PackageError::UnsafePath { why: "has a drive prefix", .. }));
    assert!(matches!(with("a\\x"), PackageError::UnsafePath { .. }));
    assert!(matches!(with("PLUGIN.wasm"), PackageError::Duplicate(a, b) if a == "plugin.wasm" && b == "PLUGIN.wasm"));
    // "é" composed and decomposed are one name.
    let mut entries = files(false, &component);
    entries.push(("caf\u{e9}.txt".into(), b"1".to_vec()));
    entries.push(("cafe\u{301}.txt".into(), b"2".to_vec()));
    assert!(matches!(open(&zip_of(&entries)), Err(PackageError::Duplicate(..))));
    // Full case folding, not lower-casing: each pair is one name ("ß" folds to "ss", the "ﬁ" ligature to "fi", the
    // long "ſ" to "s").
    for (one, other) in
        [("stra\u{df}e.txt", "STRASSE.txt"), ("\u{fb01}le.txt", "FILE.txt"), ("\u{17f}ee.txt", "see.txt")]
    {
        let mut entries = files(false, &component);
        entries.push((one.into(), b"1".to_vec()));
        entries.push((other.into(), b"2".to_vec()));
        assert!(matches!(open(&zip_of(&entries)), Err(PackageError::Duplicate(..))), "{one} and {other}");
    }

    let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    for (name, bytes) in files(false, &component) {
        writer.start_file(name, SimpleFileOptions::default()).unwrap();
        writer.write_all(&bytes).unwrap();
    }
    writer.add_symlink("link.wasm", "/etc/passwd", SimpleFileOptions::default()).unwrap();
    let symlink = writer.finish().unwrap().into_inner();
    assert_eq!(open(&symlink).unwrap_err(), PackageError::NotRegular("link.wasm".into()));

    assert_eq!(open(&zip_of(&files(false, &component)[1..])).unwrap_err(), PackageError::MissingManifest);
    let no_component: Vec<_> = files(false, &component).into_iter().filter(|(p, _)| p != "plugin.wasm").collect();
    assert_eq!(open(&zip_of(&no_component)).unwrap_err(), PackageError::MissingComponent("plugin.wasm".into()));
    let native = vec![
        ("plugin.toml".into(), manifest_text(false, "[requests]\nscopes = [\"native\"]\n").into_bytes()),
        ("plugin.wasm".into(), component.clone()),
    ];
    assert!(matches!(open(&zip_of(&native)), Err(PackageError::Manifest(m)) if m.contains("plugin.native_refused")));
}

#[test]
fn a_bomb_is_refused_before_it_expands_past_the_limit() {
    let limits = PackageLimits { max_expanded_bytes: 1024 * 1024, ..PackageLimits::default() };
    let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let deflated = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    writer.start_file("plugin.toml", deflated).unwrap();
    writer.write_all(&vec![b' '; 8 * 1024 * 1024]).unwrap();
    let bomb = writer.finish().unwrap().into_inner();
    assert!(bomb.len() < 64 * 1024, "a small archive: {} bytes", bomb.len());
    let error = PluginPackage::open_zip(&bomb, &limits).unwrap_err();
    assert!(matches!(error, PackageError::TooLarge { what: "max_expanded_bytes", .. }), "{error}");
    let many: Vec<(String, Vec<u8>)> = (0..300).map(|i| (format!("f{i}"), Vec::new())).collect();
    assert!(matches!(open(&zip_of(&many)), Err(PackageError::TooLarge { what: "max_files", .. })));
}

#[test]
fn staging_writes_only_inside_its_fresh_folder_and_hashes_what_it_wrote() {
    let component = common::ok_component();
    let package = open(&signed_zip(&component, false)).unwrap();
    let root = tempfile::tempdir().unwrap();
    let staging = root.path().join("staging");
    let (dir, hash) = package.stage(&staging).unwrap();
    assert_eq!(hash, package.hash);
    assert!(dir.starts_with(&staging));
    let top: Vec<_> = std::fs::read_dir(root.path()).unwrap().map(|e| e.unwrap().file_name()).collect();
    assert_eq!(top, ["staging"], "nothing outside staging");
    assert_eq!(std::fs::read(dir.join("plugin.wasm")).unwrap(), component);
    assert!(dir.join("plugin.sig").is_file(), "the signature is staged with the files");
}

#[test]
fn a_staged_and_an_installed_signed_package_verify_again_when_reopened() {
    let package = open(&signed_zip(&common::ok_component(), false)).unwrap();
    let PublisherCheck::Verified(key) = package.publisher() else {
        panic!("the fixture is signed: {:?}", package.publisher())
    };
    let root = tempfile::tempdir().unwrap();
    let (dir, _) = package.stage(&root.path().join("staging")).unwrap();
    let reopened = PluginPackage::open_folder(&dir, &PackageLimits::default()).unwrap();
    assert_eq!((reopened.hash, reopened.publisher()), (package.hash, PublisherCheck::Verified(key)));

    let installed = InstallRoot::new(root.path().join("data")).install(&package).unwrap();
    let reopened = PluginPackage::open_folder(&installed.dir, &PackageLimits::default()).unwrap();
    assert_eq!(reopened.publisher(), PublisherCheck::Verified(key), "the installed copy is still signed");
}

#[test]
fn installs_keep_the_last_two_versions() {
    let root = tempfile::tempdir().unwrap();
    let installs = InstallRoot::new(root.path());
    let mut hashes = Vec::new();
    for i in 0..3u8 {
        let mut entries = files(false, &common::ok_component());
        entries.push(("note.txt".into(), vec![i]));
        let installed = installs.install(&open(&zip_of(&entries)).unwrap()).unwrap();
        assert!(installed.dir.join("plugin.wasm").is_file());
        hashes.push(installed.hash);
    }
    let id = omuzaki_plugin_api::PluginId::new("example").unwrap();
    assert_eq!(installs.versions(&id), hashes[1..]);
    assert!(!installs.plugin_dir(&id).join(hashes[0].to_string()).exists(), "the oldest version is gone");
    assert!(installs.plugin_dir(&id).join(hashes[2].to_string()).exists());
}

#[test]
fn approvals_attach_to_hash_and_signer_and_a_dev_key_stays_on_its_machine() {
    let here = MachineId("machine-a".into());
    let there = MachineId("machine-b".into());
    let dev = KeyId([7; 32]);
    let publisher = KeyId([9; 32]);
    let h = |b: u8| ContentHash([b; 32]);
    let dev_approval = ApprovalKey::of(h(1), &PublisherCheck::Verified(dev), &[dev], &here).unwrap();
    assert_eq!(dev_approval.signer, Signer::Dev { key: dev, machine: here.clone() });
    let rebuilt = ApprovalKey::of(h(2), &PublisherCheck::Verified(dev), &[dev], &here).unwrap();
    assert!(dev_approval.covers(&rebuilt, &here), "a rebuild on the same machine keeps its approval");
    assert!(!dev_approval.covers(&rebuilt, &there), "never on another machine");
    let by_publisher = ApprovalKey::of(h(1), &PublisherCheck::Verified(publisher), &[dev], &here).unwrap();
    let update = ApprovalKey::of(h(3), &PublisherCheck::Verified(publisher), &[dev], &there).unwrap();
    assert!(by_publisher.covers(&update, &there), "an update by the same publisher keeps the approval");
    assert!(!by_publisher.covers(&dev_approval, &here));
    let unsigned = ApprovalKey::of(h(1), &PublisherCheck::Unsigned, &[], &here).unwrap();
    assert!(
        unsigned.covers(&unsigned, &there) && !unsigned.covers(&ApprovalKey { hash: h(2), ..unsigned.clone() }, &here)
    );
    assert!(matches!(ApprovalKey::of(h(1), &PublisherCheck::Mismatch, &[], &here), Err(PackageError::Signature(_))));
}

#[test]
fn a_corrupt_cache_file_is_compiled_again() {
    let (engine, _) = common::engine();
    let dir = tempfile::tempdir().unwrap();
    let cache = CodeCache::new(dir.path());
    let bytes = common::ok_component();
    let hash = ContentHash([5; 32]);
    assert_eq!(cache.load(&engine, &hash, &bytes).unwrap().1, CacheOutcome::Compiled);
    assert_eq!(cache.load(&engine, &hash, &bytes).unwrap().1, CacheOutcome::Hit);
    let file = cache.file_for(&engine, &hash);
    let mut contents = std::fs::read(&file).unwrap();
    let last = contents.len() - 1;
    contents[last] ^= 0xff;
    std::fs::write(&file, &contents).unwrap();
    assert_eq!(cache.load(&engine, &hash, &bytes).unwrap().1, CacheOutcome::Compiled, "a bad digest recompiles");
    std::fs::write(&file, b"short").unwrap();
    assert_eq!(cache.load(&engine, &hash, &bytes).unwrap().1, CacheOutcome::Compiled, "a truncated file recompiles");
    assert_eq!(cache.load(&engine, &hash, &bytes).unwrap().1, CacheOutcome::Hit);
}

#[test]
fn installing_a_package_end_to_end_and_refusing_a_bad_signature() {
    let world = World::new();
    let engine = Arc::new(
        omuzaki_plugin::PluginEngine::new(omuzaki_plugin::HostConfig::default(), world.clock.clone()).unwrap(),
    );
    let plugins = WasmPlugins::new(engine, world.sessions.clone(), Grants::person(), None);
    let root = tempfile::tempdir().unwrap();
    let storage = Storage {
        install: InstallRoot::new(root.path().join("data")),
        cache: CodeCache::new(root.path().join("cache")),
    };
    let path = root.path().join("example.zip");
    std::fs::write(&path, signed_zip(&common::ok_component(), false)).unwrap();
    assert_eq!(
        plugins.install(&path, &storage, &omuzaki_plugin_mock::conformance::agent()),
        Err(PluginError::denied(omuzaki_ops_api::Scope::EditSettings))
    );
    let info = plugins.install(&path, &storage, &person()).unwrap();
    assert_eq!(info.state, PluginState::Installed);
    assert_eq!(plugins.enable(&info.id, &person()).map(|i| i.state), Ok(PluginState::Running));

    let mut entries = files(true, &common::ok_component());
    let sig = signature_of(&entries);
    entries[0].1.extend_from_slice(b"description = \"changed after signing\"\n");
    entries.push(("plugin.sig".into(), sig));
    let bad = root.path().join("bad.zip");
    std::fs::write(&bad, zip_of(&entries)).unwrap();
    assert!(matches!(plugins.install(&bad, &storage, &person()), Err(PluginError::Signature(_))));
}

#[test]
fn an_install_approves_nothing_and_a_refused_one_changes_nothing_on_disk() {
    let world = World::new();
    let engine = Arc::new(
        omuzaki_plugin::PluginEngine::new(omuzaki_plugin::HostConfig::default(), world.clock.clone()).unwrap(),
    );
    let plugins = WasmPlugins::new(engine, world.sessions.clone(), Grants::person(), None);
    let root = tempfile::tempdir().unwrap();
    let storage = Storage {
        install: InstallRoot::new(root.path().join("data")),
        cache: CodeCache::new(root.path().join("cache")),
    };
    let package = |note: u8| {
        let mut entries = files(false, &common::ok_component());
        entries[0].1 = manifest_text(false, "[requests]\nscopes = [\"read\"]\n").into_bytes();
        entries.push(("note.txt".into(), vec![note]));
        let path = root.path().join(format!("example-{note}.zip"));
        std::fs::write(&path, zip_of(&entries)).unwrap();
        path
    };
    let info = plugins.install(&package(1), &storage, &person()).unwrap();
    assert_eq!(info.approved, [], "nothing is approved until the person confirms the card");
    let id = info.id.clone();
    let versions = storage.install.versions(&id);
    assert_eq!(versions.len(), 1);

    assert!(matches!(plugins.install(&package(2), &storage, &person()), Err(PluginError::Manifest(_))));
    assert_eq!(storage.install.versions(&id), versions, "the refused version was never copied in");
    let dirs =
        std::fs::read_dir(storage.install.plugin_dir(&id)).unwrap().filter(|e| e.as_ref().unwrap().path().is_dir());
    assert_eq!(dirs.count(), 1);
    let cached = std::fs::read_dir(root.path().join("cache").join("plugins")).unwrap().count();
    assert_eq!(cached, 1, "the refused package's compiled code was not kept");
}

#[test]
fn a_failed_install_leaves_nothing_in_staging() {
    let root = tempfile::tempdir().unwrap();
    let installs = InstallRoot::new(root.path());
    let package = open(&zip_of(&files(false, &common::ok_component()))).unwrap();
    // A file where the plugin's folder goes: making that folder fails after staging.
    let in_the_way = installs.plugin_dir(&package.manifest.id);
    std::fs::create_dir_all(in_the_way.parent().unwrap()).unwrap();
    std::fs::write(&in_the_way, b"in the way").unwrap();
    assert!(matches!(installs.install(&package), Err(PackageError::Io(_))));
    let staging = in_the_way.parent().unwrap().join(".staging");
    let staged: Vec<_> = std::fs::read_dir(staging).unwrap().collect();
    assert!(staged.is_empty(), "{staged:?}");
}

#[test]
fn two_installs_of_one_id_at_once_copy_one_version() {
    let world = World::new();
    let engine = Arc::new(
        omuzaki_plugin::PluginEngine::new(omuzaki_plugin::HostConfig::default(), world.clock.clone()).unwrap(),
    );
    let plugins = Arc::new(WasmPlugins::new(engine, world.sessions.clone(), Grants::person(), None));
    let root = tempfile::tempdir().unwrap();
    let storage = Arc::new(Storage {
        install: InstallRoot::new(root.path().join("data")),
        cache: CodeCache::new(root.path().join("cache")),
    });
    let paths: Vec<_> = (0..2u8)
        .map(|note| {
            let mut entries = files(false, &common::ok_component());
            entries.push(("note.txt".into(), vec![note]));
            let path = root.path().join(format!("example-{note}.zip"));
            std::fs::write(&path, zip_of(&entries)).unwrap();
            path
        })
        .collect();
    let gate = Arc::new(std::sync::Barrier::new(2));
    let threads: Vec<_> = paths
        .into_iter()
        .map(|path| {
            let (plugins, storage, gate) = (plugins.clone(), storage.clone(), gate.clone());
            std::thread::spawn(move || {
                gate.wait();
                plugins.install(&path, &storage, &person())
            })
        })
        .collect();
    let results: Vec<_> = threads.into_iter().map(|t| t.join().unwrap()).collect();
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1, "{results:?}");
    let id = omuzaki_plugin_api::PluginId::new("example").unwrap();
    assert_eq!(storage.install.versions(&id).len(), 1, "the refused install copied nothing");
}

/// A folder with the package's files written out.
#[cfg(unix)]
fn folder_of(entries: &[(String, Vec<u8>)]) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    for (name, bytes) in entries {
        let path = dir.path().join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }
    dir
}

#[cfg(unix)]
#[test]
fn a_folder_with_a_hardlink_or_a_non_utf8_name_is_refused() {
    use std::os::unix::ffi::OsStrExt;
    let limits = PackageLimits::default();
    let entries = files(false, &common::ok_component());
    let linked = folder_of(&entries);
    std::fs::hard_link(linked.path().join("plugin.wasm"), linked.path().join("again.wasm")).unwrap();
    assert!(matches!(PluginPackage::open_folder(linked.path(), &limits), Err(PackageError::NotRegular(_))));

    let odd = folder_of(&entries);
    let name = std::ffi::OsStr::from_bytes(b"bad-\xff.txt");
    std::fs::write(odd.path().join(name), b"x").unwrap();
    let refused = PluginPackage::open_folder(odd.path(), &limits);
    assert!(matches!(refused, Err(PackageError::UnsafePath { why: "is not UTF-8", .. })), "{refused:?}");
}
