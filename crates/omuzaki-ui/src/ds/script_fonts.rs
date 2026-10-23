//! The faces a language needs beyond the bundled Latin ones (`design-system.md` section 3, #205): Japanese, Korean and
//! Simplified Chinese text is drawn in Noto Sans JP, KR or SC, fetched on first need and never committed (ADR 0003).
//!
//! The files are the ones the stage's fallback chain pins (`omuzaki_stage::sheet::layout::CATALOG`: exact commit URLs,
//! sizes and SHA-256), fetched through the same verified download cache, so the studio and the character sheet share one
//! reviewed copy. Nothing is fetched or loaded for a language the bundled faces already cover: an English session
//! never maps a CJK font into memory. A face that arrives goes into every family's chain right after the bundled face,
//! so a CJK character is drawn in it and a Latin one stays in Instrument Sans.

use std::sync::Arc;
use std::sync::mpsc::{Receiver, TryRecvError, channel};

use egui::{Context, FontData};
use omuzaki_l10n_api::Locale;
use omuzaki_stage::sheet::layout::{CATALOG, Family, FontSource, Weight};

use super::fonts;

/// One face being fetched: its family, and where its bytes (or why there are none) arrive.
struct Loading {
    family: Family,
    done: Receiver<Result<Vec<u8>, String>>,
}

/// The faces fetched for the languages the studio has spoken in this session.
pub struct ScriptFonts {
    source: Arc<dyn FontSource>,
    loading: Option<Loading>,
    installed: Vec<(Family, Arc<FontData>)>,
    /// Why the last fetch failed: the text falls back to egui's own faces, and the Settings window can say why.
    failed: Option<String>,
}

impl std::fmt::Debug for ScriptFonts {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let installed: Vec<Family> = self.installed.iter().map(|(family, _)| *family).collect();
        f.debug_struct("ScriptFonts")
            .field("installed", &installed)
            .field("failed", &self.failed)
            .finish_non_exhaustive()
    }
}

impl ScriptFonts {
    /// Faces from `source`: the app passes the stage's `PinnedFonts` over the shared verified cache.
    pub fn new(source: Arc<dyn FontSource>) -> Self {
        Self { source, loading: None, installed: Vec::new(), failed: None }
    }

    /// The family `locale`'s words are drawn in, when the bundled faces lack its script.
    pub fn family_for(locale: &Locale) -> Option<Family> {
        match locale.as_str().split('-').next() {
            Some("ja") => Some(Family::Jp),
            Some("ko") => Some(Family::Kr),
            Some("zh") => Some(Family::Sc),
            _ => None,
        }
    }

    /// Starts fetching the face `locale` needs, on a thread of its own, unless it is here, on its way, or not needed.
    pub fn want(&mut self, locale: &Locale) {
        let Some(family) = Self::family_for(locale) else { return };
        if self.installed.iter().any(|(f, _)| *f == family) || self.loading.as_ref().is_some_and(|l| l.family == family)
        {
            return;
        }
        let Some(spec) = CATALOG.iter().find(|spec| spec.family == family && spec.weight == Weight::Regular) else {
            return;
        };
        let (tx, done) = channel();
        let source = Arc::clone(&self.source);
        let spawned = std::thread::Builder::new().name("script-font".into()).spawn(move || {
            // The studio may close while a face downloads; nobody is listening then, and that is fine.
            let _ = tx.send(source.load(spec).map_err(|e| e.to_string()));
        });
        match spawned {
            Ok(_) => self.loading = Some(Loading { family, done }),
            Err(e) => self.failed = Some(e.to_string()),
        }
    }

    /// Takes a face that arrived and puts every face fetched so far into `ctx`'s fonts; true when that happened. One
    /// empty channel poll when nothing is on its way.
    pub fn poll(&mut self, ctx: &Context) -> bool {
        let Some(loading) = &self.loading else { return false };
        let answer = match loading.done.try_recv() {
            Ok(answer) => answer,
            Err(TryRecvError::Empty) => return false,
            Err(TryRecvError::Disconnected) => Err("the font fetch stopped".to_owned()),
        };
        let family = loading.family;
        self.loading = None;
        match answer {
            Ok(bytes) => {
                self.installed.push((family, Arc::new(FontData::from_owned(bytes))));
                self.failed = None;
                let faces: Vec<(String, Arc<FontData>)> = self
                    .installed
                    .iter()
                    .map(|(family, data)| (format!("script-face-{family:?}"), Arc::clone(data)))
                    .collect();
                ctx.set_fonts(fonts::with_fallbacks(&faces));
                true
            }
            Err(reason) => {
                tracing::warn!(%reason, ?family, "a script's font could not be fetched; its text uses egui's own faces");
                self.failed = Some(reason);
                false
            }
        }
    }

    /// Waits for the face on its way, then takes it as [`poll`](Self::poll) would. For tests: the studio never blocks.
    pub fn finish(&mut self, ctx: &Context) -> bool {
        if let Some(loading) = &self.loading
            && let Ok(answer) = loading.done.recv()
        {
            let (tx, done) = channel();
            let _ = tx.send(answer);
            self.loading = Some(Loading { family: loading.family, done });
        }
        self.poll(ctx)
    }

    /// The families fetched and in use.
    pub fn installed(&self) -> Vec<Family> {
        self.installed.iter().map(|(family, _)| *family).collect()
    }

    /// Whether a face is on its way.
    pub const fn loading(&self) -> bool {
        self.loading.is_some()
    }

    /// Why the last fetch failed, if it did.
    pub fn failed(&self) -> Option<&str> {
        self.failed.as_deref()
    }
}
