//! `output.window.open`, `close`, `set`, `status` and `suggest_key`: the output window and its modes as ops.
//!
//! **Modes** ([`WindowMode`]): `studio` shows the character only in the studio (no window), `capture` opens the clean
//! window a capture tool grabs, and `desktop` puts the character alone on the desktop (transparent, on top, out of the
//! taskbar, clicked through on its transparent pixels). They are general blocks, not use cases: a stream, a companion
//! on the desktop and an AI co-host are the same modes mixed with who drives the character. `output.window.status`
//! lists which modes this machine can show and why not; asking for one it can't is refused with that reason. Every
//! click passing through (`click_through: full`) is the person's call: an agent's request waits for them, since the
//! person could lose the character on their desktop.
//!
//! The ops decide with [`crate::window`] (the exact size, the key colour, the transparent fallback) and drive a
//! [`WindowHost`] the app injects; the Live UI implements the host and builds the real window. Opening and closing are
//! **live actions**: kind `Edit` with `Undo::None` and scope `perform`, so an agent has no grant for them by default and
//! the document is never touched. The two queries need `read` only.
//!
//! A dry run plans the window and reports what would open without calling the host.
//!
//! **Settings:** the Machine setting `output.window` ([`OutputWindow`]) keeps the person's last open request (mode,
//! backdrop, key colour, size, title, stacking, opacity, click-through). `output.window.open` writes it when a person
//! opens the window and the app has a settings service, so the Live workspace shows the same choices after a restart;
//! an agent's request opens the window without becoming the person's choice. The window itself never reopens on its
//! own (`docs/design/settings.md`: anything that starts output is never restored).

use omuzaki_core::colour::Colour;
use omuzaki_ops_api::{Mode, Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use omuzaki_settings_api::error::SettingError;
use omuzaki_settings_api::meta::{AgentWrite, Apply, InProfile, SettingMeta, SettingScope};
use omuzaki_settings_api::register_setting;
use omuzaki_settings_api::service::{Layer, Settings, SettingsExt as _};
use omuzaki_settings_api::setting::Setting;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use omuzaki_stream_api::window::{
    Backdrop, ClickThrough, DEFAULT_KEY, DESKTOP_SCALE, DesktopSettings, MAX_SCALE, MIN_SCALE, ModeSupport,
    PaletteSource, SizePolicy, WindowError, WindowHost, WindowMode, WindowSettings, WindowState,
};

use crate::window::{check_title, pick_key, plan};

/// The output window's last open request, as the Machine setting `output.window`: what the Live workspace shows as the
/// window's backdrop, key colour, size, title and stacking after a restart. Machine scope (the next computer has other
/// displays and capture tools), outside profiles, applied live (it is read when the window next opens). An agent's
/// write waits for the person: these choices decide what a capture tool sees.
pub struct OutputWindow;

impl Setting for OutputWindow {
    type Value = Open;
    const META: SettingMeta = SettingMeta::new(
        "output.window",
        "The output window's mode, backdrop, key colour, size, title, stacking, opacity and click-through, as last opened",
        SettingScope::Machine,
        AgentWrite::Confirm,
        InProfile::Excluded,
        Apply::Live,
        1,
    );

    fn default() -> Open {
        Open::default()
    }

    /// The same rules as the op's arguments: a title `check_title` accepts, a scale from [`MIN_SCALE`] to
    /// [`MAX_SCALE`] and an opacity from `MIN_OPACITY` to 1.
    fn check(value: &Open) -> Result<(), SettingError> {
        let key = Self::META.key;
        if let Some(title) = &value.title {
            check_title(title).map_err(|e| SettingError::invalid(key, e.to_string()))?;
        }
        if let Some(opacity) = value.opacity {
            DesktopSettings::new(opacity, ClickThrough::default())
                .check()
                .map_err(|e| SettingError::invalid(key, e.to_string()))?;
        }
        match value.scale {
            Some(scale) if !(MIN_SCALE..=MAX_SCALE).contains(&scale) => {
                Err(SettingError::invalid(key, WindowError::Scale(scale).to_string()))
            }
            _ => Ok(()),
        }
    }
}

register_setting!(OutputWindow);

/// What to put behind the character.
#[derive(Copy, Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BackdropKind {
    /// A flat key colour (`key`), which every capture tool can remove. The default.
    #[default]
    Key,
    /// A transparent surface. A host that cannot make one gets the key colour instead (`fallback: true`).
    Transparent,
}

/// Show the character in a window: for a capture tool to grab (`capture`, the default), alone on the desktop
/// (`desktop`), or only in the studio (`studio`, which closes the window).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [
    {},
    { "backdrop": "key", "key": "#ff00ff", "scale": 0.5, "title": "Omuzaki Output" },
    { "backdrop": "transparent", "always_on_top": true },
    { "mode": "desktop", "scale": 0.4, "opacity": 0.9 },
    { "mode": "studio" },
]))]
pub struct Open {
    /// `capture` (default): the clean window a capture tool grabs. `desktop`: the character alone on the desktop,
    /// transparent, above other windows, out of the taskbar, dragged by the character. `studio`: no window; the
    /// character stays in the studio. `output.window.status` says which this machine can show.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<WindowMode>,
    /// `desktop` only: how opaque the character is, `0.1` to `1`. Default `1`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opacity: Option<f32>,
    /// `desktop` only: `transparent` (default) lets clicks on empty pixels through while the character takes its own;
    /// `off` takes every click; `full` lets every click through, and only a person may turn it on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub click_through: Option<ClickThrough>,
    /// `key` (default) or `transparent`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backdrop: Option<BackdropKind>,
    /// The key colour, `#rrggbb`. Default `#00b140`; call `output.window.suggest_key` for one that cannot be
    /// confused with the cast. With `transparent`, it is the colour used if the host cannot do transparency.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<Colour>,
    /// Size the window at this factor (`0.1` to `2.0`) of the output: below `1` a smaller preview, above `1` a request
    /// for a larger window. Each side is capped at the window size limit with the aspect ratio kept, so an output
    /// already at the limit opens at the limit. Default: exactly the output size.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scale: Option<f32>,
    /// The window title, 1 to 64 characters, no control characters. Default `Omuzaki Output`; capture tools find the
    /// window by it, so change it only when a tool needs another.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Keep the window above other windows. Default `false`. A host that cannot is answered with
    /// `always_on_top: false`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub always_on_top: Option<bool>,
}

/// Change an open desktop window's opacity or click-through, without reopening it.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{ "opacity": 0.6 }, { "click_through": "off" }]))]
pub struct Set {
    /// How opaque the character is, `0.1` to `1`. Default: as it is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub opacity: Option<f32>,
    /// `transparent`, `off` or `full` (a person's call). Default: as it is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub click_through: Option<ClickThrough>,
}

/// Close the clean output window. Does nothing when it is closed.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{}]))]
pub struct Close {}

/// Report whether the output window is open, its mode, size, backdrop and whether it is minimised, and which modes this
/// machine can show.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{}]))]
pub struct Status {}

/// Suggest the key colour that cannot be confused with the cast's palette.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{}, { "scene": "main" }]))]
pub struct SuggestKey {
    /// The scene whose cast to check. Default: the scene on air.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scene: Option<String>,
}

fn window_error(field: &str, error: &WindowError) -> OpError {
    match error {
        WindowError::Host(_) => OpError::conflict(error.to_string()).hint("check the output window, then ask again"),
        // The host's own surface is unusable: nothing in the request can repair it.
        WindowError::ScaleFactor(_) | WindowError::EmptyOutput(..) => OpError::conflict(error.to_string())
            .hint("the display or the output reports an unusable size; check them, then ask again"),
        WindowError::Unsupported(_) => OpError::unsupported(error.to_string()),
        _ => OpError::invalid(field, error.to_string()),
    }
}

/// `desktop`, checked; an agent's full click-through waits for a person, in a dry run too.
fn checked_desktop(ctx: &OpCtx<'_>, desktop: DesktopSettings) -> Result<DesktopSettings, OpError> {
    desktop.check().map_err(|e| window_error("opacity", &e))?;
    if desktop.click_through == ClickThrough::Full {
        ctx.require_person("let every click pass through the character on the desktop")?;
    }
    Ok(desktop)
}

/// Refuses `mode` with the host's reason when this machine can't show it.
fn require_mode(host: &dyn WindowHost, mode: WindowMode) -> Result<(), OpError> {
    let support = host.modes().into_iter().find(|s| s.mode == mode);
    match support {
        Some(ModeSupport { available: true, .. }) => Ok(()),
        Some(ModeSupport { reason, .. }) => {
            let reason = reason.unwrap_or_else(|| format!("This computer can't show the character {}", where_(mode)));
            Err(OpError::unsupported(reason).hint("output.window.status lists the modes this machine can show"))
        }
        None => Err(OpError::unsupported(format!("This build can't show the character {}", where_(mode)))),
    }
}

/// Where `mode` shows the character, for a reason a person reads.
const fn where_(mode: WindowMode) -> &'static str {
    match mode {
        WindowMode::Studio => "only in the studio",
        WindowMode::Capture => "in a window for capture",
        _ => "on the desktop",
    }
}

impl Open {
    /// The settings this request asks for, with the defaults filled in.
    fn settings(&self) -> Result<WindowSettings, OpError> {
        let mut settings = WindowSettings::default();
        settings.mode = self.mode.unwrap_or_default();
        let desktop_only = |field: &str| {
            OpError::invalid(field, format!("{field} applies to the desktop mode")).hint("add \"mode\": \"desktop\"")
        };
        if settings.mode != WindowMode::Desktop {
            if self.opacity.is_some() {
                return Err(desktop_only("opacity"));
            }
            if self.click_through.is_some() {
                return Err(desktop_only("click_through"));
            }
        }
        if let Some(title) = &self.title {
            check_title(title).map_err(|e| window_error("title", &e))?;
            settings.title.clone_from(title);
        }
        let key = self.key.unwrap_or(DEFAULT_KEY);
        settings.backdrop = match self.backdrop.unwrap_or_default() {
            BackdropKind::Key => Backdrop::Key(key),
            BackdropKind::Transparent => Backdrop::Transparent,
        };
        if let Some(scale) = self.scale {
            settings.size = SizePolicy::Scale(scale);
        }
        settings.always_on_top = self.always_on_top.unwrap_or(false);
        if settings.mode == WindowMode::Desktop {
            // The desktop mode is these, whatever else was asked: the character alone, above the person's work, with
            // the pointer theirs.
            settings.backdrop = Backdrop::Transparent;
            settings.always_on_top = true;
            settings.hide_cursor = false;
            settings.size = SizePolicy::Scale(self.scale.unwrap_or(DESKTOP_SCALE));
            settings.desktop =
                DesktopSettings::new(self.opacity.unwrap_or(1.0), self.click_through.unwrap_or_default());
        }
        Ok(settings)
    }
}

impl Op for Open {
    fn meta() -> OpMeta {
        OpMeta {
            name: "output.window.open",
            summary: "Show the character in a window: the clean window for a capture tool (the output's exact size and a key colour by default), alone on the desktop (transparent, on top, clicked through), or only in the studio.",
            kind: OpKind::Edit,
            scope: Scope::Perform,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let mut settings = self.settings()?;
        let host = ctx.service::<dyn WindowHost>()?;
        require_mode(host, settings.mode)?;
        if settings.mode == WindowMode::Studio {
            // The studio is the window: none to open, and the one that is open closes.
            let saved = save(ctx, self)? && !ctx.dry_run;
            let was_open = host.state() != WindowState::Closed;
            if !ctx.dry_run {
                host.close();
            }
            return Ok(Plan::answer(json!({
                "mode": WindowMode::Studio,
                "opened": false,
                "closed": !ctx.dry_run,
                "was_open": was_open,
                "saved": saved,
            })));
        }
        if settings.mode == WindowMode::Desktop {
            settings.desktop = checked_desktop(ctx, settings.desktop)?;
        }
        let surface = host.surface();
        let window = plan(surface.size, settings.size, surface.scale_factor).map_err(|e| window_error("scale", &e))?;
        let caps = host.capabilities();
        if settings.mode == WindowMode::Desktop && !(caps.transparent && caps.always_on_top) {
            // A key-coloured box on the desktop is no character: the host's mode list and its capabilities disagree.
            return Err(OpError::unsupported(
                "This computer can't make a transparent window that stays on top, which the desktop needs",
            ));
        }
        let fallback = settings.backdrop == Backdrop::Transparent && !caps.transparent;
        if fallback {
            settings.backdrop = Backdrop::Key(self.key.unwrap_or(DEFAULT_KEY));
        }
        // Resolved like the backdrop: the host is never asked for what it said it cannot do.
        settings.always_on_top &= caps.always_on_top;
        // A dry run checks the write and reports `saved: false`, as it reports `opened: false`.
        let saved = save(ctx, self)? && !ctx.dry_run;
        let opened = !ctx.dry_run;
        if opened {
            host.open(&window, &settings).map_err(|e| window_error("backdrop", &e))?;
        }
        let desktop = (settings.mode == WindowMode::Desktop).then_some(settings.desktop);
        Ok(Plan::answer(json!({
            "mode": settings.mode,
            "opened": opened,
            "saved": saved,
            "desktop": desktop,
            "fallback": fallback,
            "title": settings.title,
            "backdrop": settings.backdrop,
            "always_on_top": settings.always_on_top,
            "physical": window.physical,
            "logical": window.logical,
            "scale_factor": window.scale_factor,
        })))
    }
}

/// Writes `request` to the [`OutputWindow`] setting, in the run's mode, when a person asked (or confirmed) and the
/// app has a settings service. Returns whether it was saved: `false` without a service, so the choice holds until the
/// app closes, and `false` for an agent nobody confirmed, whose request opens the window without becoming the person's
/// remembered choice (the setting is `AgentWrite::Confirm`).
fn save(ctx: &OpCtx<'_>, request: &Open) -> Result<bool, OpError> {
    let Ok(settings) = ctx.service::<dyn Settings>() else { return Ok(false) };
    if ctx.require_person("save the output window's choices").is_err() {
        return Ok(false);
    }
    let mode = if ctx.dry_run { Mode::DryRun } else { Mode::Apply };
    settings
        .set::<OutputWindow>(request, Layer::Base, mode, ctx.actor)
        .map(|_| true)
        .map_err(|e| OpError::internal(format!("the setting `{}` could not be saved: {e}", OutputWindow::META.key)))
}

impl Op for Close {
    fn meta() -> OpMeta {
        OpMeta {
            name: "output.window.close",
            summary: "Close the clean output window.",
            kind: OpKind::Edit,
            scope: Scope::Perform,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let host = ctx.service::<dyn WindowHost>()?;
        let was_open = host.state() != WindowState::Closed;
        if !ctx.dry_run {
            host.close();
        }
        Ok(Plan::answer(json!({ "closed": !ctx.dry_run, "was_open": was_open })))
    }
}

impl Op for Set {
    fn meta() -> OpMeta {
        OpMeta {
            name: "output.window.set",
            summary: "Change the character's opacity or click-through on the desktop, without reopening its window.",
            kind: OpKind::Edit,
            scope: Scope::Perform,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let host = ctx.service::<dyn WindowHost>()?;
        let WindowState::Open { mode: WindowMode::Desktop, desktop, .. } = host.state() else {
            return Err(OpError::conflict("no window is open on the desktop")
                .hint("open one with output.window.open { \"mode\": \"desktop\" }"));
        };
        let now = desktop.unwrap_or_default();
        let desktop =
            DesktopSettings::new(self.opacity.unwrap_or(now.opacity), self.click_through.unwrap_or(now.click_through));
        let desktop = checked_desktop(ctx, desktop)?;
        if !ctx.dry_run {
            host.set_desktop(&desktop).map_err(|e| window_error("opacity", &e))?;
        }
        Ok(Plan::answer(json!({ "applied": !ctx.dry_run, "desktop": desktop })))
    }
}

impl Op for Status {
    fn meta() -> OpMeta {
        OpMeta {
            name: "output.window.status",
            summary: "Say whether the output window is open, its mode, size and backdrop, and which modes this machine can show and why not.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let host = ctx.service::<dyn WindowHost>()?;
        let mut answer = serde_json::to_value(host.state())
            .map_err(|e| OpError::internal(format!("serialising the window state failed: {e}")))?;
        answer["modes"] = serde_json::to_value(host.modes())
            .map_err(|e| OpError::internal(format!("serialising the window modes failed: {e}")))?;
        Ok(Plan::answer(answer))
    }
}

impl Op for SuggestKey {
    fn meta() -> OpMeta {
        OpMeta {
            name: "output.window.suggest_key",
            summary: "Suggest the key colour that cannot be confused with the cast's palette, and how safe it is.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let palette = ctx.service::<dyn PaletteSource>()?.colours(self.scene.as_deref());
        let choice = pick_key(&palette);
        // An empty palette has an infinite margin, which JSON cannot hold: it reads as `null`.
        let margin = choice.margin.is_finite().then_some(choice.margin);
        Ok(Plan::answer(json!({ "key": choice.colour, "margin": margin, "collides": choice.collides })))
    }
}

register_op!(Open);
register_op!(Close);
register_op!(Set);
register_op!(Status);
register_op!(SuggestKey);
