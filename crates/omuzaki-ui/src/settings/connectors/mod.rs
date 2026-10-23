//! The Connectors page of the Settings window: the apps Omuzaki works with, one at a time, OBS Studio (#1030, BR-19),
//! `VTube Studio` (#1446, CN-13) and the generators (#1185, CN-05). Each part reads its app's ops and changes nothing
//! except through ops run as the person, with one exception: the OBS password and the generators' keys, which no op
//! carries (#784), go straight to the keychain the app hands the window ([`Keychain`]).

pub mod generators;
pub mod obs;
pub mod vts;

use std::sync::Arc;

use egui::Ui;
use omuzaki_ops_api::{Actor, Grants, Mode, OpError, Request, Runner};
use omuzaki_secrets_api::prelude::SecretStore;
use serde_json::Value;

use self::generators::GeneratorsPage;
use self::obs::ObsPage;
use self::vts::VtsPage;
use super::text::{Word, say};
use crate::ds::tokens::space;
use crate::ds::widgets::{Segment, Segmented};

/// The keychain the app keeps its secrets in, shared with the shell as a view model so the Settings window can store
/// the OBS password without an op carrying it.
#[derive(Clone)]
pub struct Keychain(pub Arc<dyn SecretStore>);

/// Which app the page is on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Connector {
    /// OBS Studio.
    #[default]
    Obs,
    /// `VTube Studio`.
    Vts,
    /// The generators: image, texture, voice, music and model services (`connect.*`).
    Generators,
}

/// The page's state, kept while the Settings window is open.
#[derive(Default)]
pub struct ConnectorsPage {
    connector: Connector,
    obs: ObsPage,
    vts: VtsPage,
    generators: GeneratorsPage,
}

impl ConnectorsPage {
    /// The page, storing the OBS password in `keychain` (without one, the password field says it can't be saved).
    #[must_use]
    pub fn with_keychain(mut self, keychain: Option<Keychain>) -> Self {
        self.obs = ObsPage::new(keychain.clone());
        self.generators = GeneratorsPage::new(keychain);
        self
    }

    /// The app on show.
    pub const fn connector(&self) -> Connector {
        self.connector
    }

    /// Shows `connector`.
    pub fn show_connector(&mut self, connector: Connector) {
        self.connector = connector;
    }

    /// The OBS half.
    pub fn obs(&mut self) -> &mut ObsPage {
        &mut self.obs
    }

    /// The `VTube Studio` half.
    pub fn vts(&mut self) -> &mut VtsPage {
        &mut self.vts
    }

    /// The generators half.
    pub fn generators(&mut self) -> &mut GeneratorsPage {
        &mut self.generators
    }

    /// Draws the page; `now` is egui's clock.
    pub fn show(&mut self, ui: &mut Ui, runner: &Arc<dyn Runner>, actor: &Actor, now: f64) {
        let ctx = ui.ctx().clone();
        let label = say(&ctx, Word::Connectors, &[]);
        let (obs, vts) = (say(&ctx, Word::ObsTitle, &[]), say(&ctx, Word::VtsTitle, &[]));
        let generators = crate::panels::text::say(&crate::ds::theme::ds(&ctx), crate::generate::text::GENERATORS, &[]);
        let apps = [
            Segment::new(Connector::Obs, &obs),
            Segment::new(Connector::Vts, &vts),
            Segment::new(Connector::Generators, &generators),
        ];
        ui.add(Segmented::new(&label, &mut self.connector, &apps).id_salt("settings.connectors.app"));
        ui.add_space(space::S8);
        match self.connector {
            Connector::Obs => self.obs.show(ui, runner, actor, now),
            Connector::Vts => self.vts.show(ui, runner, actor, now),
            Connector::Generators => self.generators.show(ui, runner, actor, now),
        }
    }
}

/// Runs `op` as the person and returns its answer.
///
/// # Errors
/// The op's refusal.
pub fn run(runner: &dyn Runner, actor: &Actor, op: &str, args: Value) -> Result<Value, OpError> {
    let outcome = runner.run(Request {
        op: op.to_owned(),
        args,
        mode: Mode::Apply,
        actor: actor.clone(),
        grants: Grants::person(),
        cause: None,
        tx: None,
        base: None,
    })?;
    Ok(outcome.plan.output.unwrap_or(Value::Null))
}
