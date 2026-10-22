//! The built-in voice presets (BR-24): a starting point a person or an agent copies into a character's `voice.toml`
//! with `audio.fx.preset.save` and tunes from there, and the chains the §10 budgets are measured on.
//!
//! Each starts with the clean-up every live voice wants (a gate against room noise, a high-pass against rumble and
//! plosives, a compressor for an even level) and adds its character on top.

use omuzaki_audio_api::FxPreset;
use omuzaki_audio_api::graph::NodeSpec;
use omuzaki_audio_api::ids::{NodeId, NodeKindId};
use omuzaki_audio_api::node::NodeConfig;

/// A node of kind `kind` with the parameters `params`, enabled. `None` only for an id that is not a slug, which the
/// tests below rule out for every built-in.
fn node(id: &str, kind: &str, params: &[(&str, f32)]) -> Option<NodeSpec> {
    Some(NodeSpec {
        id: NodeId::new(id).ok()?,
        kind: NodeKindId::new(kind).ok()?,
        config: NodeConfig::default(),
        params: params.iter().map(|(name, value)| ((*name).to_owned(), *value)).collect(),
        enabled: true,
        sidechain: None,
    })
}

fn gate() -> Option<NodeSpec> {
    node("gate", "fx.gate", &[])
}

/// A high-pass at 80 Hz and a gentle presence lift.
fn eq() -> Option<NodeSpec> {
    node(
        "eq",
        "fx.eq",
        &[
            ("b1.type", 1.0),
            ("b1.freq", 80.0),
            ("b2.type", 4.0),
            ("b2.freq", 3000.0),
            ("b2.gain_db", 2.0),
            ("b2.q", 0.9),
        ],
    )
}

fn compressor() -> Option<NodeSpec> {
    node("compressor", "fx.compressor", &[("makeup_db", 3.0)])
}

fn pitch(semitones: f32, formant: f32) -> Option<NodeSpec> {
    node("pitch", "fx.pitch", &[("semitones", semitones), ("formant", formant)])
}

fn preset(name: &str, chain: Vec<Option<NodeSpec>>) -> Option<FxPreset> {
    Some(FxPreset { name: name.to_owned(), chain: chain.into_iter().collect::<Option<Vec<_>>>()? })
}

/// The built-in presets, in the order a list shows them.
#[must_use]
pub fn builtin() -> Vec<FxPreset> {
    [
        preset("Clean", vec![gate(), eq(), compressor()]),
        preset("Deeper", vec![gate(), eq(), pitch(-4.0, 0.88), compressor()]),
        preset("Higher", vec![gate(), eq(), pitch(4.0, 1.12), compressor()]),
        preset("Giant", vec![gate(), eq(), pitch(-9.0, 0.78), compressor()]),
        preset("Tiny", vec![gate(), eq(), pitch(8.0, 1.3), compressor()]),
        preset(
            "Robot",
            vec![
                gate(),
                eq(),
                node("pitch", "fx.pitch", &[("monotone_hz", 110.0)]),
                node("robot", "fx.robot", &[]),
                compressor(),
            ],
        ),
        preset("Radio", vec![gate(), node("radio", "fx.radio", &[("mode", 1.0), ("noise_db", -48.0)]), compressor()]),
        preset("Telephone", vec![gate(), node("radio", "fx.radio", &[("mode", 0.0)]), compressor()]),
    ]
    .into_iter()
    .flatten()
    .collect()
}

/// The chain the `voice.fx` budget is measured on (spec §10): every BR-24 effect on except pitch and formant, in the
/// order a voice runs through them.
#[must_use]
pub fn budget_chain() -> Vec<NodeSpec> {
    [gate(), eq(), compressor(), node("robot", "fx.robot", &[]), node("radio", "fx.radio", &[])]
        .into_iter()
        .flatten()
        .collect()
}

/// The chain of `voice.fx.pitch` and `voice.fx.cpu`: [`budget_chain`] with the pitch and formant shifter on as well.
#[must_use]
pub fn budget_chain_with_pitch() -> Vec<NodeSpec> {
    // The shifter goes after the clean-up and before the compressor, as in the pitched presets.
    [gate(), eq(), pitch(5.0, 1.1), compressor(), node("robot", "fx.robot", &[]), node("radio", "fx.radio", &[])]
        .into_iter()
        .flatten()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_builtin_is_complete() {
        assert_eq!(builtin().len(), 8);
        assert!(builtin().iter().all(|p| p.chain.len() >= 3));
        assert_eq!(budget_chain().len(), 5);
        let with_pitch = budget_chain_with_pitch();
        let kinds = with_pitch.iter().map(|n| n.kind.as_str()).collect::<Vec<_>>();
        assert_eq!(kinds, ["fx.gate", "fx.eq", "fx.pitch", "fx.compressor", "fx.robot", "fx.radio"]);
    }
}
