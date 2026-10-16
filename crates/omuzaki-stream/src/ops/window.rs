//! `output.window.open`, `close`, `status` and `suggest_key`: the clean output window as ops.
//!
//! The ops decide with [`crate::window`] (the exact size, the key colour, the transparent fallback) and drive a
//! [`WindowHost`] the app injects; the Live UI implements the host and builds the real window. Opening and closing are
//! **live actions**: kind `Edit` with `Undo::None` and scope `perform`, so an agent has no grant for them by default and
//! the document is never touched. The two queries need `read` only.
//!
//! A dry run plans the window and reports what would open without calling the host.

use omuzaki_core::colour::Colour;
use omuzaki_ops_api::{Op, OpCtx, OpError, OpKind, OpMeta, Plan, Scope, Undo, register_op};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::service::{PaletteSource, WindowHost};
use crate::window::{
    Backdrop, DEFAULT_KEY, SizePolicy, WindowError, WindowSettings, WindowState, check_title, pick_key, plan,
};

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
pub struct Open {
    /// `key` (default) or `transparent`.
    #[serde(default)]
    pub backdrop: Option<BackdropKind>,
    /// The key colour, `#rrggbb`. Default `#00b140`; call `output.window.suggest_key` for one that cannot be
    /// confused with the cast. With `transparent`, it is the colour used if the host cannot do transparency.
    #[serde(default)]
    pub key: Option<Colour>,
    /// Make the window this fraction (`0.1` to `2.0`) of the output size. Default: exactly the output size.
    #[serde(default)]
    pub scale: Option<f32>,
    /// The window title, 1 to 64 characters, no control characters. Default `Omuzaki Output`; capture tools find the
    /// window by it, so change it only when a tool needs another.
    #[serde(default)]
    pub title: Option<String>,
}

/// Close the clean output window. Does nothing when it is closed.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Close {}

/// Report whether the clean output window is open, and its size and backdrop.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Status {}

/// Suggest the key colour that cannot be confused with the cast's palette.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SuggestKey {
    /// The scene whose cast to check. Default: the scene on air.
    #[serde(default)]
    pub scene: Option<String>,
}

fn window_error(field: &str, error: &WindowError) -> OpError {
    match error {
        WindowError::Host(_) => OpError::conflict(error.to_string()).hint("check the output window, then ask again"),
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
        let fallback = settings.backdrop == Backdrop::Transparent && !host.capabilities().transparent;
        if fallback {
            settings.backdrop = Backdrop::Key(self.key.unwrap_or(DEFAULT_KEY));
        }
        let opened = !ctx.dry_run;
        if opened {
            host.open(&window, &settings).map_err(|e| window_error("backdrop", &e))?;
        }
        Ok(Plan::answer(json!({
            "opened": opened,
            "fallback": fallback,
            "title": settings.title,
            "backdrop": settings.backdrop,
            "physical": window.physical,
            "logical": window.logical,
            "scale_factor": window.scale_factor,
        })))
    }
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
            summary: "Say whether the clean output window is open, and its size and backdrop.",
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
