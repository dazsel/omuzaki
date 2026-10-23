//! Animation exporters and their self-registration (`docs/design/io-api.md` D11): a second, small registry beside
//! the model one in `format.rs`, so a motion format never has to pretend to be a model.
//!
//! A format crate registers its exporter with [`register_animation_exporter!`](crate::register_animation_exporter)
//! next to the implementation; [`run_export_animation`] adds the checks, the rights rules and strict mode every
//! format shares.

use crate::check::animation_rights;
use crate::{ExportCtx, ExportOptions, ExportOutcome, FormatInfo, IoAnimation, IoError, Report};

/// Writes one motion format from an [`IoAnimation`]. Register with
/// [`register_animation_exporter!`](crate::register_animation_exporter).
///
/// Like a model [`Exporter`](crate::Exporter), it must be deterministic: equal animations give equal bytes whatever the
/// order of their tracks, and nothing of the time or the build is written.
pub trait AnimationExporter: Send + Sync {
    /// Static facts about the format.
    fn info(&self) -> FormatInfo;

    /// Format-specific checks (the VRMA exporter: node and extension rules). Not the shared ones: those run in
    /// [`run_export_animation`].
    fn check(&self, animation: &IoAnimation, options: &ExportOptions) -> Report;

    /// Writes the animation. Called only after the checks passed.
    ///
    /// # Errors
    /// [`IoError::Rejected`] for a track the format cannot encode, [`IoError::Cancelled`], [`IoError::Internal`].
    fn export(&self, animation: &IoAnimation, ctx: &ExportCtx<'_>) -> Result<ExportOutcome, IoError>;
}

/// Registers an animation exporter type. `T` must be `Default`.
pub struct AnimationExporterRegistration {
    /// Builds an instance.
    pub create: fn() -> Box<dyn AnimationExporter>,
}

fn make<T: AnimationExporter + Default + 'static>() -> Box<dyn AnimationExporter> {
    Box::new(T::default())
}

impl AnimationExporterRegistration {
    /// The registration for `T`. A `const fn` so
    /// [`register_animation_exporter!`](crate::register_animation_exporter) works at link time.
    pub const fn of<T: AnimationExporter + Default + 'static>() -> Self {
        Self { create: make::<T> }
    }
}

inventory::collect!(AnimationExporterRegistration);

/// Every registered animation exporter, in link order.
pub fn animation_exporters() -> impl Iterator<Item = Box<dyn AnimationExporter>> {
    inventory::iter::<AnimationExporterRegistration>.into_iter().map(|r| (r.create)())
}

/// The animation exporter for `format`.
pub fn find_animation_exporter(format: &str) -> Option<Box<dyn AnimationExporter>> {
    animation_exporters().find(|e| e.info().id == format)
}

/// Every animation format with an exporter, sorted by id. Separate from [`formats`](crate::formats) on purpose: the
/// model list's `FormatSupport` is not `#[non_exhaustive]`, so it cannot grow an "animation" flag.
pub fn animation_formats() -> Vec<FormatInfo> {
    let mut out: Vec<FormatInfo> = animation_exporters().map(|e| e.info()).collect();
    out.sort_by_key(|f| f.id);
    out.dedup_by_key(|f| f.id);
    out
}

/// The checks that must hold before any animation exporter runs: the animation's invariants
/// ([`IoAnimation::check`]) and the rights rules (D11, "Rights"). Exposed so a UI can show what would block an export
/// without choosing a format first.
pub fn preflight_animation(animation: &IoAnimation, options: &ExportOptions) -> Report {
    let mut report = animation.check();
    report.extend(animation_rights(animation, options));
    report
}

/// Exports `animation` with `exporter`: [`preflight_animation`], then the exporter's own
/// [`AnimationExporter::check`]; if anything is an error (or a warning in strict mode) nothing is written and the whole
/// report comes back in [`IoError::Rejected`]. Otherwise the exporter runs and its outcome carries the preflight's
/// findings and its own notes together.
///
/// # Errors
/// [`IoError::Rejected`] with every finding, or whatever the exporter returns.
pub fn run_export_animation(
    exporter: &dyn AnimationExporter,
    animation: &IoAnimation,
    ctx: &ExportCtx<'_>,
) -> Result<ExportOutcome, IoError> {
    let mut report = preflight_animation(animation, ctx.options);
    report.extend(exporter.check(animation, ctx.options));
    if ctx.options.strict {
        report = report.strict();
    }
    if report.has_errors() {
        return Err(IoError::Rejected(report));
    }
    ctx.progress.checkpoint("export", 0.0)?;
    let mut outcome = exporter.export(animation, ctx)?;
    report.extend(std::mem::take(&mut outcome.report));
    outcome.report = report;
    Ok(outcome)
}

/// Registers an animation exporter type: `register_animation_exporter!(VrmaExporter);`. Put it next to the type.
#[macro_export]
macro_rules! register_animation_exporter {
    ($ty:ty) => {
        $crate::__inventory::submit! { $crate::AnimationExporterRegistration::of::<$ty>() }
        $crate::__record_registration!("animation_exporter", $ty);
    };
}
