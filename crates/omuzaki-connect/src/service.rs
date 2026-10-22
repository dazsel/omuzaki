//! `impl Connectors for Host`: the service any crate's op reaches with `ctx.service::<dyn Connectors>()`.

use omuzaki_ai_api::{Progress, Requester};
use omuzaki_connect_api::{
    Call, ConnectorId, ConnectorManifest, Connectors, InputRights, Preview, PreviewError, Request, RunEnv, RunError,
    RunResult, Status,
};

use crate::host::Host;

impl Connectors for Host {
    fn list(&self) -> Vec<ConnectorManifest> {
        self.parts.set.iter().map(|e| e.manifest.clone()).collect()
    }

    fn describe(&self, id: &ConnectorId) -> Option<ConnectorManifest> {
        self.parts.set.get(id).map(|e| e.manifest.clone())
    }

    fn status(&self, id: &ConnectorId) -> Option<Status> {
        let entry = self.parts.set.get(id)?;
        Some(self.status_of(&self.resolve(entry)))
    }

    fn preview(
        &self,
        req: &Request,
        connector: Option<&ConnectorId>,
        who: Requester,
        inputs: &InputRights,
    ) -> Result<Preview, PreviewError> {
        Host::preview(self, req, connector, who, inputs)
    }

    fn run(&self, call: &Call, env: &RunEnv<'_>) -> Result<RunResult, RunError> {
        Host::run(self, call, env)
    }

    fn ping(&self, id: &ConnectorId, progress: &dyn Progress) -> Result<(), RunError> {
        Host::ping(self, id, progress)
    }
}
