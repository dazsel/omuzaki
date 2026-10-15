//! The eight registered fakes, so a link gate and the host see them: each is a `Default` newtype around a
//! [`FakeConnector`] (what `register_connector!` needs), billing what its manifest says per output.

use omuzaki_connect_api::{
    ConnectError, Connector, ConnectorConfig, ConnectorManifest, Estimate, Request, RunCx, RunOutput, Target,
    register_connector,
};
use omuzaki_ops_api::Services;

use crate::fake::FakeConnector;
use crate::script::Script;

/// Declares a newtype over a [`FakeConnector`] built by `$make` with `$script`, forwarding every method.
macro_rules! mock {
    ($(#[$doc:meta])* $name:ident, $make:ident, $script:expr) => {
        $(#[$doc])*
        pub struct $name(pub FakeConnector);

        impl Default for $name {
            fn default() -> Self {
                Self(FakeConnector::$make($script))
            }
        }

        impl Connector for $name {
            fn target(&self, m: &ConnectorManifest, c: &ConnectorConfig, s: &Services) -> Result<Target, ConnectError> {
                self.0.target(m, c, s)
            }

            fn estimate(&self, m: &ConnectorManifest, t: &Target, req: &Request) -> Result<Estimate, ConnectError> {
                self.0.estimate(m, t, req)
            }

            fn run(&self, m: &ConnectorManifest, req: &Request, cx: &RunCx<'_>) -> Result<RunOutput, ConnectError> {
                self.0.run(m, req, cx)
            }

            fn ping(&self, m: &ConnectorManifest, cx: &RunCx<'_>) -> Result<(), ConnectError> {
                self.0.ping(m, cx)
            }
        }
    };
}

mock!(
    /// `mock-image`: free, local PNG images.
    MockImage,
    image,
    Script::default()
);
mock!(
    /// `mock-image-paid`: PNG images that play a hosted service at $0.04 each.
    MockImagePaid,
    image,
    Script::priced(40_000)
);
mock!(
    /// `mock-texture`: free, local PNG textures.
    MockTexture,
    texture,
    Script::default()
);
mock!(
    /// `mock-3d`: one-triangle GLB models that play a hosted service at $0.20 each.
    Mock3d,
    model3d,
    Script::priced(200_000)
);
mock!(
    /// `mock-voice`: WAV speech that plays a hosted service at $0.03 a thousand characters, billed as its manifest
    /// prices the text.
    MockVoice,
    voice,
    Script::by_manifest()
);
mock!(
    /// `mock-music`: free, local WAV music and sound effects.
    MockMusic,
    music,
    Script::default()
);
mock!(
    /// `mock-motion`: free, local BVH motion.
    MockMotion,
    motion,
    Script::default()
);
mock!(
    /// `mock-dcc`: a free, local DCC program that echoes its call as JSON.
    MockDcc,
    dcc,
    Script::default()
);

register_connector!("mock-image", include_str!("../connectors/mock-image.connector.toml"), MockImage);
register_connector!("mock-image-paid", include_str!("../connectors/mock-image-paid.connector.toml"), MockImagePaid);
register_connector!("mock-texture", include_str!("../connectors/mock-texture.connector.toml"), MockTexture);
register_connector!("mock-3d", include_str!("../connectors/mock-3d.connector.toml"), Mock3d);
register_connector!("mock-voice", include_str!("../connectors/mock-voice.connector.toml"), MockVoice);
register_connector!("mock-music", include_str!("../connectors/mock-music.connector.toml"), MockMusic);
register_connector!("mock-motion", include_str!("../connectors/mock-motion.connector.toml"), MockMotion);
register_connector!("mock-dcc", include_str!("../connectors/mock-dcc.connector.toml"), MockDcc);
