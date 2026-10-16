//! `output.window.open`, `close`, `status` and `suggest_key`: the clean output window as ops.
//!
//! The ops decide with [`crate::window`] (the exact size, the key colour, the transparent fallback) and drive a
//! [`WindowHost`] the app injects; the Live UI implements the host and builds the real window. Opening and closing are
//! **live actions**: kind `Edit` with `Undo::None` and scope `perform`, so an agent has no grant for them by default and
//! the document is never touched. The two queries need `read` only.
//!
//! A dry run plans the window and reports what would open without calling the host.
//!
//! **Settings:** the Machine setting `output.window` ([`OutputWindow`]) keeps the person's last open request (backdrop,
//! key colour, size, title, stacking). `output.window.open` writes it when the app has a settings service, so the Live
//! workspace shows the same choices after a restart; the window itself never reopens on its own
//! (`docs/design/settings.md`: anything that starts output is never restored).

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
    Backdrop, DEFAULT_KEY, MAX_SCALE, MIN_SCALE, PaletteSource, SizePolicy, WindowError, WindowHost, WindowSettings,
    WindowState,
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
        "The clean output window's backdrop, key colour, size, title and stacking, as last opened",
        SettingScope::Machine,
        AgentWrite::Confirm,
        InProfile::Excluded,
        Apply::Live,
        1,
    );

    fn default() -> Open {
        Open::default()
    }

    /// The same rules as the op's arguments: a title `check_title` accepts and a scale from [`MIN_SCALE`] to
    /// [`MAX_SCALE`].
    fn check(value: &Open) -> Result<(), SettingError> {
        let key = Self::META.key;
        if let Some(title) = &value.title {
            check_title(title).map_err(|e| SettingError::invalid(key, e.to_string()))?;
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

/// Open the clean output window for a capture tool to grab.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [
    {},
    { "backdrop": "key", "key": "#ff00ff", "scale": 0.5, "title": "Omuzaki Output" },
    { "backdrop": "transparent", "always_on_top": true },
]))]
pub struct Open {
    /// `key` (default) or `transparent`.
    #[serde(default)]
    pub backdrop: Option<BackdropKind>,
    /// The key colour, `#rrggbb`. Default `#00b140`; call `output.window.suggest_key` for one that cannot be
    /// confused with the cast. With `transparent`, it is the colour used if the host cannot do transparency.
    #[serde(default)]
    pub key: Option<Colour>,
    /// Size the window at this factor (`0.1` to `2.0`) of the output: below `1` a smaller preview, above `1` a request
    /// for a larger window. Each side is capped at the window size limit with the aspect ratio kept, so an output
    /// already at the limit opens at the limit. Default: exactly the output size.
    #[serde(default)]
    pub scale: Option<f32>,
    /// The window title, 1 to 64 characters, no control characters. Default `Omuzaki Output`; capture tools find the
    /// window by it, so change it only when a tool needs another.
    #[serde(default)]
    pub title: Option<String>,
    /// Keep the window above other windows. Default `false`. A host that cannot is answered with
    /// `always_on_top: false`.
    #[serde(default)]
    pub always_on_top: Option<bool>,
}

/// Close the clean output window. Does nothing when it is closed.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("examples" = [{}]))]
pub struct Close {}

/// Report whether the clean output window is open, and its size, backdrop and whether it is minimised.
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
    #[serde(default)]
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

impl Open {
    /// The settings this request asks for, with the defaults filled in.
    fn settings(&self) -> Result<WindowSettings, OpError> {
        let mut settings = WindowSettings::default();
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
        Ok(settings)
    }
}

impl Op for Open {
    fn meta() -> OpMeta {
        OpMeta {
            name: "output.window.open",
            summary: "Open the clean output window for a capture tool: the output's exact size and a key colour backdrop by default, or a scaled size and a transparent backdrop.",
            kind: OpKind::Edit,
            scope: Scope::Perform,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let mut settings = self.settings()?;
        let host = ctx.service::<dyn WindowHost>()?;
        let surface = host.surface();
        let window = plan(surface.size, settings.size, surface.scale_factor).map_err(|e| window_error("scale", &e))?;
        let caps = host.capabilities();
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
        Ok(Plan::answer(json!({
            "opened": opened,
            "saved": saved,
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

/// Writes `request` to the [`OutputWindow`] setting, in the run's mode, when the app has a settings service. Returns
/// whether it was saved: `false` without a service, so the choice holds until the app closes.
fn save(ctx: &OpCtx<'_>, request: &Open) -> Result<bool, OpError> {
    let Ok(settings) = ctx.service::<dyn Settings>() else { return Ok(false) };
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

impl Op for Status {
    fn meta() -> OpMeta {
        OpMeta {
            name: "output.window.status",
            summary: "Say whether the clean output window is open, and its size, backdrop and whether it is minimised.",
            kind: OpKind::Query,
            scope: Scope::Read,
            undo: Undo::None,
        }
    }

    fn plan(&self, ctx: &OpCtx<'_>) -> Result<Plan, OpError> {
        let state = ctx.service::<dyn WindowHost>()?.state();
        let state = serde_json::to_value(state)
            .map_err(|e| OpError::internal(format!("serialising the window state failed: {e}")))?;
        Ok(Plan::answer(state))
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
register_op!(Status);
register_op!(SuggestKey);
