//! A trivial face baker for tests. It registers itself, so [`face_baker`](omuzaki_face_api::face_baker) finds one in the
//! walking skeleton, and it bakes every channel as inert: what a face with nothing to paint produces.

use omuzaki_face_api::{BakerInput, FaceBakerFactory, register_face_baker};
use omuzaki_io_api::{
    Avatar, BakeReport, BakeRequest, ChannelBake, Expression, ExpressionBaker, Finding, IoError, IoScene, Progress,
    Realisation,
};

/// Builds [`MockFaceBaker`]s for any character. Registered with the face registry at link time.
#[derive(Default)]
pub struct MockFaceBakerFactory;

impl FaceBakerFactory for MockFaceBakerFactory {
    fn create(&self, _input: BakerInput<'_>) -> Result<Box<dyn ExpressionBaker>, IoError> {
        Ok(Box::new(MockFaceBaker))
    }
}

register_face_baker!(MockFaceBakerFactory);

/// Bakes each requested channel as an empty expression and reports it inert. The mesh is left untouched, so an export
/// of the result carries the channel names but no morph targets.
pub struct MockFaceBaker;

impl ExpressionBaker for MockFaceBaker {
    fn bake(
        &self,
        scene: &mut IoScene,
        request: &BakeRequest,
        _progress: &dyn Progress,
    ) -> Result<BakeReport, IoError> {
        const REASON: &str = "the mock face paints nothing";
        let mut report = BakeReport::default();
        for channel in &request.channels {
            report.channels.push(ChannelBake {
                channel: channel.to_string(),
                realisation: Realisation::Inert { reason: REASON.into() },
            });
            report.findings.push(Finding::warning("perfect_sync.inert_channel", format!("{channel}: {REASON}")));
        }
        let expressions = request.channels.iter().map(|channel| (channel.clone(), Expression::new()));
        scene.avatar.get_or_insert_with(Avatar::new).expressions.extend(expressions);
        Ok(report)
    }
}
