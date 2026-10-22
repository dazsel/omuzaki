//! The `VTube Studio` (VTS) plugin API as plain data: message types, validators and limits for the CN-13 bridge.
//!
//! The bridge itself: [`client::VtsClient`] (the session: discovery, authentication, reconnects, the clean degrade on
//! revoke), [`inject`] (Omuzaki's channels into VTS parameters, one batched message per frame) and [`hotkeys`] (hotkeys
//! first, expressions only when mapped). The `testing` feature adds a loopback mock server (below).
//!
//! # Clean-room
//!
//! The types are written from the documented wire protocol only: the `DenchiSoft/VTubeStudio` README (the API
//! sections and the Events page) and `Files/ErrorID.cs`, read on 2026-10-13 under the VTS EULA v5.2 (2024-04-15).
//! No text or code was copied from them or from any client library. Terms and naming rules:
//! `docs/research/2026-10-vts-plugin-api-terms.md`. Not affiliated with or endorsed by `DenchiSoft` or `Live2D` Inc.
//!
//! # Modules
//!
//! | Module | Holds |
//! |---|---|
//! | [`protocol`] | the [`Envelope`](protocol::Envelope), [`ApiError`](protocol::ApiError), [`ErrorKind`](protocol::ErrorKind) and every request, response and event |
//! | [`client`] | [`VtsClient`](client::VtsClient), [`BridgeStatus`](client::BridgeStatus), [`TokenStore`](client::TokenStore) and discovery |
//! | [`inject`] | [`ChannelMap`](inject::ChannelMap), [`Injector`](inject::Injector), the pacer and the batches |
//! | [`hotkeys`] | [`VtsMapping`](hotkeys::VtsMapping), [`suggest`](hotkeys::suggest) and the [`HotkeyLimiter`](hotkeys::HotkeyLimiter) |
//! | [`clock`] | the injected [`Clock`](clock::Clock) |
//! | [`service`] | [`VtsService`](service::VtsService), the app's one bridge, and [`VtsFeed`](service::VtsFeed), the perform loop's end |
//! | [`ops`] | `vts.connect`, `vts.disconnect`, `vts.status`, `vts.map`, `vts.suggest`, `vts.trigger`, `vts.release` |
//! | [`limits`] | the API limits as constants and the validated newtypes ([`CustomParamName`](limits::CustomParamName), [`ParamValue`](limits::ParamValue), [`FadeTime`](limits::FadeTime)) |
//! | `testing` (feature `testing`, and this crate's tests) | an in-process mock `VTube Studio` server on loopback for tests of the bridge: no VTS, account or network needed; the only module that does I/O |
#![deny(missing_docs)]

pub mod client;
pub mod clock;
pub mod hotkeys;
pub mod inject;
pub mod limits;
pub mod ops;
pub mod protocol;
pub mod service;
#[cfg(any(test, feature = "testing"))]
pub mod testing;

omuzaki_core::define_link!();

/// The plugin name shown in the user's permission popup (3 to 32 characters on the wire).
///
/// Never change it: the user's authentication token is tied to the name and the developer, so a change invalidates
/// every stored token and shows the popup again.
pub const PLUGIN_NAME: &str = "Omuzaki";

/// The plugin developer shown in the user's permission popup (3 to 32 characters on the wire).
///
/// Never change it, for the same reason as [`PLUGIN_NAME`].
pub const PLUGIN_DEVELOPER: &str = "Omuzaki Studio";

// Both strings are ASCII, so `len` is the character count. A bad edit fails the build, not the user's login.
const _: () = {
    assert!(PLUGIN_NAME.len() >= 3 && PLUGIN_NAME.len() <= 32);
    assert!(PLUGIN_DEVELOPER.len() >= 3 && PLUGIN_DEVELOPER.len() <= 32);
};
