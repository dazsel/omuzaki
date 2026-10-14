//! The behaviour every [`Store`] must have, as checks an implementation's tests call (`run_all`, or one at a time for
//! a sharper failure message). Both `MemStore` and `omuzaki-store` run them, which is what "same semantics" means.
//!
//! These are test helpers: a violated expectation panics with a message naming it, so the functions may `unwrap`.
// A panic is how a check fails, and the `# Panics` section on each function says so.
#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::sync::Barrier;
use std::thread;
use std::time::{Duration, SystemTime};

use omuzaki_store_api::{
    Commercial, Consent, ContentHash, GateError, Provenance, Removal, Source, Store, StoreError, StoreGc, licence_gate,
};

/// BLAKE3 of the empty input, from the BLAKE3 reference test vectors: proves the right hash function is in use.
const EMPTY_BLAKE3: &str = "af1349b9f5f9a1a6a0404dea36dcc9499bcb25c9adc112b7cc9a93cae41f3262";

/// How many times the two-thread race is repeated; each round uses fresh bytes, so each is a new race.
const RACE_ROUNDS: usize = 100;

/// Deterministic pseudo-random bytes for `label`: the same label and length always give the same bytes, and
/// different labels give different bytes, so checks never collide on one store.
pub fn payload(label: &str, len: usize) -> Vec<u8> {
    let mut bytes = vec![0; len];
    blake3::Hasher::new().update(label.as_bytes()).finalize_xof().fill(&mut bytes);
    bytes
}

/// A minimal provenance (made in Omuzaki) with the given commercial status.
pub fn sample_provenance(commercial: Commercial) -> Provenance {
    Provenance {
        source: Source::Authored,
        author: None,
        licence: "LicenseRef-Omuzaki-User".into(),
        attribution: None,
        commercial,
        consent: None,
        private_test: false,
    }
}

/// A provenance with every optional field filled, to prove nothing is dropped on the way through a store.
pub fn full_provenance() -> Provenance {
    Provenance {
        source: Source::Generated {
            tool: "comfyui".into(),
            model: "sdxl-1.0".into(),
            plan: Some("self-hosted".into()),
            prompt: Some("a calm fox, \"quoted\" and unicode: \u{1f98a}".into()),
            seed: Some(u64::MAX),
            inputs: vec![ContentHash([7; 32]), ContentHash([9; 32])],
        },
        author: Some("A. Artist".into()),
        licence: "CC-BY-4.0".into(),
        attribution: Some("Fox by A. Artist".into()),
        commercial: Commercial::Ok,
        consent: Some(Consent {
            person: "A. Person".into(),
            scope: "voice cloning".into(),
            document: Some(ContentHash([3; 32])),
        }),
        private_test: false,
    }
}

/// Runs every check below against `store`, which may already hold other assets.
///
/// # Panics
/// On the first expectation a check finds violated.
pub fn run_all(store: &dyn Store) {
    round_trip(store);
    unknown_hash_is_missing(store);
    duplicate_put_keeps_first_provenance(store);
    provenance_round_trips(store);
    licence_gate_reads_stored_provenance(store);
    concurrent_puts_agree(store);
}

/// `put` then `get`/`contains`/`provenance` give back what went in, for empty, tiny and multi-MiB inputs, and the
/// hash is BLAKE3.
///
/// # Panics
/// If any expectation is violated.
pub fn round_trip(store: &dyn Store) {
    let empty = store.put(&[], sample_provenance(Commercial::Ok)).unwrap();
    assert_eq!(empty.to_string(), EMPTY_BLAKE3, "the hash must be BLAKE3");
    assert_eq!(store.get(&empty).unwrap(), Vec::<u8>::new());

    for (label, len) in [("tiny", 1), ("small", 4096), ("multi-MiB", (1 << 20) + 1)] {
        let bytes = payload(label, len);
        let provenance = sample_provenance(Commercial::Ok);
        let hash = store.put(&bytes, provenance.clone()).unwrap();
        assert_eq!(hash, ContentHash::of(&bytes), "{label}: hash is BLAKE3 of the bytes");
        assert!(store.contains(&hash), "{label}: contains after put");
        assert_eq!(store.get(&hash).unwrap(), bytes, "{label}: bytes round trip");
        assert_eq!(store.provenance(&hash).unwrap(), provenance, "{label}: provenance round trips");
    }
}

/// A hash nobody stored is reported as [`StoreError::Missing`] by `get` and `provenance`, and `contains` is false.
///
/// # Panics
/// If any expectation is violated.
pub fn unknown_hash_is_missing(store: &dyn Store) {
    let unknown = ContentHash::of(b"never stored by any check");
    assert!(!store.contains(&unknown));
    assert_eq!(store.get(&unknown), Err(StoreError::Missing(unknown)));
    assert_eq!(store.provenance(&unknown), Err(StoreError::Missing(unknown)));
}

/// Storing the same bytes again returns the same hash and keeps the first provenance, whatever the second says.
///
/// # Panics
/// If any expectation is violated.
pub fn duplicate_put_keeps_first_provenance(store: &dyn Store) {
    let bytes = payload("duplicate", 2048);
    let first = Provenance { author: Some("first".into()), ..sample_provenance(Commercial::Ok) };
    let second = Provenance { author: Some("second".into()), ..sample_provenance(Commercial::No) };
    let a = store.put(&bytes, first.clone()).unwrap();
    let b = store.put(&bytes, second).unwrap();
    assert_eq!(a, b, "same bytes, same hash");
    assert_eq!(store.provenance(&a).unwrap(), first, "the first provenance wins");
    assert_eq!(store.get(&a).unwrap(), bytes);
}

/// Every field of a fully populated provenance survives storage.
///
/// # Panics
/// If any expectation is violated.
pub fn provenance_round_trips(store: &dyn Store) {
    let hash = store.put(&payload("full provenance", 512), full_provenance()).unwrap();
    assert_eq!(store.provenance(&hash).unwrap(), full_provenance());
}

/// [`licence_gate`] gives the right verdict on provenance read back from the store, for each way an asset can be
/// blocked and for the consent that unblocks a real person's likeness.
///
/// # Panics
/// If any expectation is violated.
pub fn licence_gate_reads_stored_provenance(store: &dyn Store) {
    let stored = |label: &str, provenance: Provenance| {
        let hash = store.put(&payload(label, 256), provenance).unwrap();
        store.provenance(&hash).unwrap()
    };

    let cleared = stored("gate ok", sample_provenance(Commercial::Ok));
    assert_eq!(licence_gate(&cleared, false), Ok(()));

    let non_commercial = stored("gate no", sample_provenance(Commercial::No));
    assert!(matches!(licence_gate(&non_commercial, false), Err(GateError::NonCommercial { .. })));

    let unknown = stored("gate unknown", sample_provenance(Commercial::Unknown));
    assert!(matches!(licence_gate(&unknown, false), Err(GateError::Unknown { .. })));

    let private = stored("gate private", Provenance { private_test: true, ..sample_provenance(Commercial::Ok) });
    assert_eq!(licence_gate(&private, false), Err(GateError::PrivateTest));

    let no_consent = stored("gate no consent", sample_provenance(Commercial::Ok));
    assert_eq!(licence_gate(&no_consent, true), Err(GateError::NoConsent));

    let consent = Consent { person: "A. Person".into(), scope: "likeness".into(), document: None };
    let consented = stored("gate consent", Provenance { consent: Some(consent), ..sample_provenance(Commercial::Ok) });
    assert_eq!(licence_gate(&consented, true), Ok(()));
}

/// Two threads store the same bytes at the same moment with different provenance, many times over: both succeed with
/// the same hash, both then read back the same provenance (one of the two), and it never changes afterwards.
///
/// # Panics
/// If any expectation is violated, or a worker thread panics.
pub fn concurrent_puts_agree(store: &dyn Store) {
    for round in 0..RACE_ROUNDS {
        let bytes = payload(&format!("race {round}"), 256 * 1024);
        let barrier = Barrier::new(2);
        let provenances = ["left", "right"]
            .map(|who| Provenance { author: Some(who.to_owned()), ..sample_provenance(Commercial::Ok) });

        // Each worker reads the provenance straight after its own put, so a store that let the second writer
        // overwrite the first would show two different values here, even though the end state looks consistent.
        let outcomes = thread::scope(|scope| {
            let workers = provenances.clone().map(|provenance| {
                let (bytes, barrier) = (&bytes, &barrier);
                scope.spawn(move || {
                    barrier.wait();
                    let hash = store.put(bytes, provenance).unwrap();
                    (hash, store.provenance(&hash).unwrap())
                })
            });
            workers.map(|worker| worker.join().unwrap_or_else(|panic| std::panic::resume_unwind(panic)))
        });

        let [(hash, winner), (other_hash, seen_by_other)] = outcomes;
        assert_eq!(hash, other_hash, "round {round}: both puts see the same hash");
        assert_eq!(winner, seen_by_other, "round {round}: both callers see the same provenance");
        assert!(provenances.contains(&winner), "round {round}: provenance is one of the two puts'");
        assert_eq!(store.get(&hash).unwrap(), bytes, "round {round}: bytes intact");
        assert_eq!(store.provenance(&hash).unwrap(), winner, "round {round}: provenance is stable");
    }
}

/// An object whose commit comes more than `grace` after its `put` began must survive a sweep straight after the
/// commit: the grace period counts from the commit, not from when the write started. `slow_put` stores one object and
/// advances the clock that `now` reads by more than `grace` before the commit lands.
///
/// # Panics
/// If the object is swept, or is kept only because the sweep's clock is behind it.
pub fn slow_commit_survives_sweep(
    store: &dyn StoreGc,
    slow_put: &dyn Fn() -> ContentHash,
    now: &dyn Fn() -> SystemTime,
    grace: Duration,
) {
    let hash = slow_put();
    let report = store.sweep(&|_| false, grace, now()).unwrap();
    assert!(!report.removed.contains(&hash), "a write longer than grace must not be swept right after its commit");
    assert!(store.contains(&hash));

    let later = now() + grace;
    let report = store.sweep(&|_| false, grace, later).unwrap();
    assert_eq!(report.removed, vec![hash], "once grace has passed since the commit, it is collected");
}

/// Removal and sweeping, as the keeper uses them: a removed object is gone and comes back on a `put`, a young object
/// survives a long grace period, and a sweep takes exactly the unkept, old objects. Run on an empty store.
///
/// # Panics
/// If any expectation is violated.
pub fn remove_and_sweep(store: &dyn StoreGc) {
    let kept = store.put(&payload("gc kept", 300), sample_provenance(Commercial::Ok)).unwrap();
    let dropped = store.put(&payload("gc dropped", 300), sample_provenance(Commercial::Ok)).unwrap();
    assert_eq!(
        store.hashes().unwrap(),
        {
            let mut both = vec![kept, dropped];
            both.sort_unstable();
            both
        },
        "hashes lists every object, sorted"
    );
    assert!(store.stored_at(&dropped).is_ok(), "a stored object has a commit time");
    assert_eq!(
        store.stored_at(&ContentHash::of(b"gc unknown")),
        Err(StoreError::Missing(ContentHash::of(b"gc unknown")))
    );

    let future = SystemTime::now() + Duration::from_hours(1);
    assert_eq!(
        store.remove_if_older(&kept, SystemTime::UNIX_EPOCH).unwrap(),
        Removal::Fresh,
        "an object committed after the cutoff is left alone"
    );
    assert_eq!(store.remove_if_older(&dropped, future).unwrap(), Removal::Removed, "old enough: removed");
    assert!(!store.contains(&dropped), "removed: not contained");
    assert_eq!(store.get(&dropped), Err(StoreError::Missing(dropped)));
    assert_eq!(store.provenance(&dropped), Err(StoreError::Missing(dropped)));
    assert_eq!(store.remove_if_older(&dropped, future).unwrap(), Removal::Absent, "removing twice reports it absent");
    assert!(!store.hashes().unwrap().contains(&dropped), "removed: not listed");
    assert!(store.contains(&kept), "remove touches only its own object");

    let again = store.put(&payload("gc dropped", 300), sample_provenance(Commercial::No)).unwrap();
    assert_eq!(again, dropped, "same bytes, same hash");
    assert_eq!(store.get(&dropped).unwrap(), payload("gc dropped", 300), "stored again, bytes intact");
    assert_eq!(store.provenance(&dropped).unwrap(), sample_provenance(Commercial::No), "fresh provenance");

    let young = store.sweep(&|_| false, Duration::from_hours(1), SystemTime::now()).unwrap();
    assert!(young.removed.is_empty(), "objects younger than the grace period survive");
    assert_eq!(young.kept, 2);

    let swept = store.sweep(&|hash| *hash == kept, Duration::ZERO, SystemTime::now()).unwrap();
    assert_eq!(swept.removed, vec![dropped], "a sweep takes the unkept, old objects");
    assert_eq!(swept.kept, 1);
    assert!(store.contains(&kept) && !store.contains(&dropped));
}
