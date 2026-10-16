//! Sheet fonts download through `HttpFetcher` and `VerifiedCache`: end to end against a local server (loopback http, no
//! outside network). The cache, the fetcher and `PinnedFonts` are the real ones; only the pinned URLs point at the
//! server.

#![allow(clippy::disallowed_methods, reason = "a test reading its own fixtures and temporary cache; not an op")]
// Integration-test helpers may panic: a panic is a clear test failure.
#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpListener};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use omuzaki_ai::VerifiedCache;
use omuzaki_ai_api::{FetchError, FileSource, NoProgress, Progress, Sha256};
use omuzaki_http::HttpFetcher;
use omuzaki_stage::sheet::layout::{
    Align, Dpi, Family, FontConfig, FontError, FontSource, FontSpec, HanVariant, Orientation, Page, Paper, PinnedFonts,
    SheetLayout, TextStyle, Theme, Weight,
};
use sha2::Digest as _;

/// A committed subset of a pinned Noto font (3 KB), served as if it were the download.
const SUBSET: &[u8] = include_bytes!("fixtures/fonts/NotoSansThai-Regular.ttf");

/// What the server answers to every request.
#[derive(Clone, Copy)]
enum Reply {
    /// These bytes, with a length.
    Body(&'static [u8]),
    /// Bytes that never end.
    Endless,
}

/// A local http server that counts requests; its threads end with the test process.
struct Server {
    addr: SocketAddr,
    hits: Arc<AtomicUsize>,
}

impl Server {
    fn start(reply: Reply) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
        let addr = listener.local_addr().expect("addr");
        let hits = Arc::new(AtomicUsize::new(0));
        let counted = hits.clone();
        std::thread::spawn(move || {
            while let Ok((mut stream, _)) = listener.accept() {
                let counted = counted.clone();
                std::thread::spawn(move || {
                    let mut request = [0u8; 2048];
                    if stream.read(&mut request).is_err() {
                        return;
                    }
                    counted.fetch_add(1, Ordering::SeqCst);
                    match reply {
                        Reply::Body(bytes) => {
                            let head = format!(
                                "HTTP/1.1 200 OK\r\ncontent-length: {}\r\nconnection: close\r\n\r\n",
                                bytes.len()
                            );
                            let _ = stream.write_all(head.as_bytes()).and_then(|()| stream.write_all(bytes));
                        }
                        Reply::Endless => {
                            if stream.write_all(b"HTTP/1.1 200 OK\r\nconnection: close\r\n\r\n").is_err() {
                                return;
                            }
                            let chunk = [b'x'; 8192];
                            while stream.write_all(&chunk).is_ok() {}
                        }
                    }
                });
            }
        });
        Self { addr, hits }
    }

    fn url(&self) -> &'static str {
        // A test-only leak: `FontSpec` holds `'static` URLs, as the catalogue's do.
        Box::leak(format!("http://{}/font.ttf", self.addr).into_boxed_str())
    }

    fn hits(&self) -> usize {
        self.hits.load(Ordering::SeqCst)
    }
}

fn sha256(bytes: &[u8]) -> Sha256 {
    Sha256(sha2::Sha256::digest(bytes).into())
}

/// A cache in `dir` over the real fetcher, with loopback http allowed and no backoff.
fn cache(dir: &Path) -> Arc<VerifiedCache<HttpFetcher>> {
    let fetcher = HttpFetcher::builder().allow_loopback_http(true).backoff(Duration::ZERO).build();
    Arc::new(VerifiedCache::new(dir, fetcher))
}

/// A spec pinned to `bytes`, fetched from `url`.
fn spec(bytes: &[u8], url: &'static str) -> FontSpec {
    let urls: &'static [&'static str] = Box::leak(vec![url].into_boxed_slice());
    FontSpec {
        family: Family::Thai,
        weight: Weight::Regular,
        file_name: "Test-Regular.ttf",
        urls,
        sha256: sha256(bytes),
        size: bytes.len() as u64,
    }
}

/// The errors a failed `get` carries (the cache wraps the per-URL failures in `AllFailed`).
fn failures(error: FetchError) -> Vec<FetchError> {
    match error {
        FetchError::AllFailed(errors) => errors,
        other => panic!("expected AllFailed, got {other:?}"),
    }
}

#[test]
fn two_gets_for_the_same_pin_cost_one_request() {
    let server = Server::start(Reply::Body(SUBSET));
    let dir = tempfile::tempdir().unwrap();
    let cache = cache(dir.path());
    let pin = spec(SUBSET, server.url()).pin();
    let first = cache.get(&pin, &NoProgress).unwrap();
    let second = cache.get(&pin, &NoProgress).unwrap();
    assert_eq!(first, second);
    assert_eq!(std::fs::read(first).unwrap(), SUBSET);
    assert_eq!(server.hits(), 1, "the second get reads the cache");
}

#[test]
fn pinned_fonts_load_the_bytes_once_across_loads() {
    let server = Server::start(Reply::Body(SUBSET));
    let dir = tempfile::tempdir().unwrap();
    let fonts = PinnedFonts::new(cache(dir.path()));
    let spec = spec(SUBSET, server.url());
    assert_eq!(fonts.load(&spec).unwrap(), SUBSET);
    assert_eq!(fonts.load(&spec).unwrap(), SUBSET);
    assert_eq!(server.hits(), 1);
}

#[test]
fn a_wrong_body_is_a_hash_error_and_nothing_is_kept() {
    let server = Server::start(Reply::Body(&[b'b'; 58]));
    let dir = tempfile::tempdir().unwrap();
    let cache = cache(dir.path());
    let reviewed = vec![b'a'; 58];
    let pin = spec(&reviewed, server.url()).pin();
    let errors = failures(cache.get(&pin, &NoProgress).unwrap_err());
    assert!(matches!(errors.as_slice(), [FetchError::Hash { .. }]), "{errors:?}");
    assert_eq!(cache.lookup(pin.sha256).unwrap(), None, "the wrong bytes are not in the cache");
}

#[test]
fn an_endless_body_is_cut_off_at_the_pin_as_too_large() {
    let server = Server::start(Reply::Endless);
    let dir = tempfile::tempdir().unwrap();
    let cache = cache(dir.path());
    let pin = spec(SUBSET, server.url()).pin();
    let errors = failures(cache.get(&pin, &NoProgress).unwrap_err());
    assert!(
        matches!(errors.as_slice(), [FetchError::TooLarge { limit, .. }] if *limit == SUBSET.len() as u64),
        "{errors:?}"
    );
    assert_eq!(cache.lookup(pin.sha256).unwrap(), None);
}

/// Records the stage names a download reports.
#[derive(Default)]
struct Stages(Mutex<Vec<String>>);

impl Progress for Stages {
    fn stage(&self, name: &str, _: f32) {
        self.0.lock().unwrap().push(name.to_owned());
    }
    fn is_cancelled(&self) -> bool {
        false
    }
}

#[test]
fn the_progress_label_names_the_font_and_its_size_in_megabytes() {
    let body: &'static [u8] = Box::leak(vec![7u8; 2_500_000].into_boxed_slice());
    let server = Server::start(Reply::Body(body));
    let dir = tempfile::tempdir().unwrap();
    let stages = Arc::new(Stages::default());
    let fonts = PinnedFonts::new(cache(dir.path())).with_progress(stages.clone());
    let spec = spec(body, server.url());
    assert_eq!(PinnedFonts::download_label(&spec), "downloading Test-Regular.ttf (3 MB)");
    // The pin is the body's own hash, so the load succeeds; the cache reports under the label, not its own stage name.
    assert_eq!(fonts.load(&spec).unwrap().len(), body.len());
    let seen = stages.0.lock().unwrap().clone();
    assert!(!seen.is_empty(), "a 2.5 MB download reports progress");
    assert!(seen.iter().all(|name| name == "downloading Test-Regular.ttf (3 MB)"), "{seen:?}");
}

/// Sends every font to `url` instead of the catalogue's, so a sheet's own font requests reach the local server.
struct Redirected {
    fonts: PinnedFonts,
    url: &'static str,
}

impl FontSource for Redirected {
    fn load(&self, wanted: &FontSpec) -> Result<Vec<u8>, FontError> {
        let urls: &'static [&'static str] = Box::leak(vec![self.url].into_boxed_slice());
        self.fonts.load(&FontSpec { urls, family: wanted.family, weight: wanted.weight, ..*wanted })
    }
}

fn draw_japanese_name(source: Redirected) -> Vec<omuzaki_stage::sheet::layout::MissingGlyph> {
    let fonts = FontConfig::new(HanVariant::Japanese, Arc::new(source));
    let mut sheet =
        SheetLayout::with_fonts(Page::paper(Paper::A4, Orientation::Portrait, Dpi::D150), Theme::light(), fonts)
            .unwrap();
    let style = TextStyle::new(24.0, Weight::Regular, omuzaki_core::colour::Colour::BLACK);
    sheet.text_on_baseline("山田", &style, (40.0, 200), Align::Left, None);
    sheet.font_report().missing
}

#[test]
fn offline_the_sheet_reports_the_missing_characters_and_the_hint_lists_them() {
    // A port nothing listens on: the connection is refused, as it is with no network.
    let dead = {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        format!("http://{}/font.otf", listener.local_addr().unwrap())
    };
    let dir = tempfile::tempdir().unwrap();
    let source = Redirected { fonts: PinnedFonts::new(cache(dir.path())), url: Box::leak(dead.into_boxed_str()) };
    let missing = draw_japanese_name(source);
    let characters: Vec<char> = missing.iter().map(|m| m.character).collect();
    assert_eq!(characters, vec!['山', '田']);
    let hint = PinnedFonts::offline_hint(&missing).expect("something is missing");
    assert_eq!(hint, "connect once to download these fonts; no font drew: 山 田");
    assert_eq!(PinnedFonts::offline_hint(&[]), None);
}

#[test]
fn a_missing_source_or_folder_is_not_cured_by_connecting() {
    // No font source at all (`latin_only`): nothing was tried over the network, so the hint must not suggest it.
    let mut sheet = SheetLayout::with_fonts(
        Page::paper(Paper::A4, Orientation::Portrait, Dpi::D150),
        Theme::light(),
        FontConfig::latin_only(),
    )
    .unwrap();
    let style = TextStyle::new(24.0, Weight::Regular, omuzaki_core::colour::Colour::BLACK);
    sheet.text_on_baseline("山田", &style, (40.0, 200), Align::Left, None);
    let missing = sheet.font_report().missing;
    assert_eq!(missing.iter().map(|m| m.character).collect::<Vec<_>>(), vec!['山', '田']);
    assert!(missing.iter().all(|m| !m.downloadable), "{missing:?}");
    assert_eq!(PinnedFonts::offline_hint(&missing).unwrap(), "no available font covers: 山 田");
}

#[test]
fn the_hint_does_not_tell_the_user_to_download_for_a_character_no_font_covers() {
    let glyph = |character, downloadable| omuzaki_stage::sheet::layout::MissingGlyph {
        character,
        reason: String::new(),
        downloadable,
    };
    assert_eq!(
        PinnedFonts::offline_hint(&[glyph('ж', false)]).unwrap(),
        "no available font covers: ж",
        "nothing to connect for"
    );
    assert_eq!(
        PinnedFonts::offline_hint(&[glyph('山', true), glyph('ж', false)]).unwrap(),
        "connect once to download these fonts; no font drew: 山; no available font covers: ж"
    );
}

#[test]
fn the_hint_cuts_a_long_list_and_counts_the_rest() {
    let missing: Vec<_> = ('a'..='z')
        .map(|character| omuzaki_stage::sheet::layout::MissingGlyph {
            character,
            reason: String::new(),
            downloadable: true,
        })
        .collect();
    let hint = PinnedFonts::offline_hint(&missing).unwrap();
    assert!(hint.ends_with("a b c d e f g h i j k l and 14 more"), "{hint}");
}
