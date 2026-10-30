//! The Omuzaki desktop app's UI (docs/design/ux/).
//!
//! Each area is one module, added by its own task (one `mod` line here), so parallel PRs never edit the same code.
#![deny(missing_docs)]

pub mod aux_window;
pub mod broadcast;
pub mod ds;
pub mod generate;
pub mod input;
pub mod live;
pub mod output_window;
pub mod panels;
pub mod script;
pub mod settings;
pub mod shell;
pub mod studio;
pub mod testing;
pub mod theatre;
pub mod wiring;

// Lets `register_story!` and `register_contrast!` record each registration for the link gate (ADR 0015).
#[doc(hidden)]
pub use omuzaki_core::__record_registration;

// The stage registers the `perform.*` and `trigger.*` ops the Live workspace runs, the stream crate the
// `output.window.*` ops its output switch runs, and the animation crate the retarget and life layer the perform thread
// builds on, so the app links all three, and the character crate, whose parts the kit character is built from. The
// character and stage contracts and the renderer self-register too (part kinds and looks, the stage's facts, shading
// programs), and the viewport draws with all three. The model registry registers the `model.*` ops,
// and the face tracker and the phone link their own (the phone's `input.phone` setting). The audio engine registers the
// `audio.*` ops, the voice effects their `fx.*` kinds and `audio.fx.*` ops, the OBS client the `obs.*` ops, the VTube
// Studio bridge the `vts.*` ops, the automation engine and its nodes the `show.*` and `automation.*` ops, Twitch the
// `twitch.*` ops, the settings store the `settings.*` ops, first-run setup the `setup.*` ops, the virtual camera its
// output kind, the OS input hub the `input.capture.*` and `input.binding.*` ops and its backends, the Theatre's book the
// `theatre.*` ops, and the script runtime the `script.*` ops the Script workspace runs (ADR 0015). The studio's own
// `ui.*` settings register here (`ds::settings_keys`).
// The studio's words in every shipped language (`locales/<lang>/omuzaki-ui.ftl`, design `l10n-ops.md` section 3.3).
omuzaki_l10n_api::register_locales!();

omuzaki_core::define_link! {
    omuzaki_ai::link();
    omuzaki_anim::link();
    omuzaki_app::link();
    omuzaki_audio_engine::link();
    omuzaki_audio_fx::link();
    omuzaki_char::link();
    omuzaki_char_api::link();
    omuzaki_cohost::link();
    omuzaki_connect::link();
    omuzaki_input::link();
    omuzaki_live::link();
    omuzaki_live_api::link();
    omuzaki_live_nodes::link();
    omuzaki_mcp::link();
    omuzaki_obs::link();
    omuzaki_overlay::link();
    omuzaki_render::link();
    omuzaki_script::link();
    omuzaki_settings::link();
    omuzaki_setup::link();
    omuzaki_short::link();
    omuzaki_stage::link();
    omuzaki_stage_api::link();
    omuzaki_stream::link();
    omuzaki_stream_share::link();
    omuzaki_theatre::link();
    omuzaki_track::link();
    omuzaki_track_net::link();
    omuzaki_twitch::link();
    omuzaki_vcam::link();
    omuzaki_vts::link();
    omuzaki_youtube::link();
}
