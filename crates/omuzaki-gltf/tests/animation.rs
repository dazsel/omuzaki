//! glTF animations (`omuzaki_gltf::animation`): the deterministic writer and the bounded reader.

use omuzaki_gltf::Document;
use omuzaki_gltf::accessor::Accessors;
use omuzaki_gltf::animation::{
    Animation, Channel, Interpolation, MAX_KEYS, Path, Sampler, read_animations, write_animation,
};
use omuzaki_gltf::buffers::{Buffers, Loader};
use omuzaki_gltf::pack::Packer;
use omuzaki_gltf::schema::Node;
use omuzaki_io_api::{IoError, Limits, NoResolver};

fn channel(node: usize, path: Path, interpolation: Interpolation, times: &[f32], values: &[f32]) -> Channel {
    Channel { node, path, sampler: Sampler { interpolation, times: times.to_vec(), values: values.to_vec() } }
}

fn clip() -> Animation {
    let t = [0.0, 0.5, 1.0];
    let half = std::f32::consts::FRAC_1_SQRT_2;
    Animation {
        name: Some("wave".into()),
        channels: vec![
            channel(0, Path::Translation, Interpolation::Linear, &t, &[0.0, 0.9, 0.0, 0.0, 0.88, 0.0, 0.0, 0.9, 0.0]),
            channel(
                1,
                Path::Rotation,
                Interpolation::Linear,
                &t,
                &[0.0, 0.0, 0.0, 1.0, 0.0, 0.0, half, half, 0.0, 0.0, 0.0, 1.0],
            ),
            channel(2, Path::Translation, Interpolation::Step, &[0.0, 0.25], &[0.0, 0.0, 0.0, 1.0, 0.0, 0.0]),
            channel(1, Path::Scale, Interpolation::Linear, &t, &[1.0; 9]),
            channel(3, Path::Weights, Interpolation::Linear, &t, &[0.0, 1.0, 0.5, 0.5, 1.0, 0.0]),
        ],
    }
}

fn document() -> Document {
    let mut doc = Document { nodes: (0..4).map(|_| Node::default()).collect(), ..Document::default() };
    doc.asset.version = "2.0".into();
    doc
}

/// Writes `animation` into a fresh document and returns the GLB.
fn write(animation: &Animation) -> Result<Vec<u8>, IoError> {
    let (mut doc, mut pack) = (document(), Packer::default());
    write_animation(&mut doc, &mut pack, animation, Some("VRMC_vrm_animation"))?;
    omuzaki_gltf::glb(doc, pack)
}

fn read(bytes: &[u8]) -> Result<(Document, Vec<Animation>), IoError> {
    let parsed = omuzaki_gltf::parse(bytes, &Limits::default())?;
    let mut loader = Loader::new(&NoResolver, u64::MAX);
    let buffers = Buffers::load(&parsed.document, parsed.chunk, &mut loader)?;
    let animations = read_animations(&parsed.document, &Accessors::new(&parsed.document, &buffers))?;
    Ok((parsed.document.clone(), animations))
}

#[test]
fn a_written_animation_reads_back_key_for_key() {
    let a = clip();
    let (_, back) = read(&write(&a).unwrap()).unwrap();
    assert_eq!(back, vec![a]);
}

#[test]
fn output_is_byte_identical_and_shares_equal_time_arrays() {
    let bytes = write(&clip()).unwrap();
    assert_eq!(bytes, write(&clip()).unwrap());
    let (doc, _) = read(&bytes).unwrap();
    let samplers = doc.animations[0]["samplers"].as_array().unwrap();
    let inputs: Vec<u64> = samplers.iter().map(|s| s["input"].as_u64().unwrap()).collect();
    assert_eq!(inputs[0], inputs[1], "equal times share one accessor");
    assert_eq!(inputs[0], inputs[3]);
    assert_ne!(inputs[0], inputs[2], "other times get their own");
    let distinct: std::collections::BTreeSet<u64> = inputs.iter().copied().collect();
    assert_eq!(distinct.len(), 2);
    for input in distinct {
        let accessor = &doc.accessors[usize::try_from(input).unwrap()];
        assert!(accessor.min.is_some() && accessor.max.is_some(), "every input accessor has min and max");
    }
    assert_eq!(doc.accessors[0].min.as_deref(), Some(&[0.0][..]));
    assert_eq!(doc.accessors[0].max.as_deref(), Some(&[1.0][..]));
    assert_eq!(samplers[2]["interpolation"], "STEP");
    assert_eq!(doc.extensions_used, ["VRMC_vrm_animation"]);
    assert!(doc.buffer_views.iter().all(|v| v.target.is_none()), "animation data is no vertex buffer");
}

#[test]
fn the_writer_refuses_what_gltf_forbids() {
    let refuse = |change: &dyn Fn(&mut Animation)| {
        let mut a = clip();
        change(&mut a);
        write(&a).unwrap_err()
    };
    assert!(matches!(
        refuse(&|a| a.channels[0].sampler.interpolation = Interpolation::CubicSpline),
        IoError::Unsupported { .. }
    ));
    for change in [
        &|a: &mut Animation| a.channels[0].sampler.values[1] = f32::NAN,
        &|a: &mut Animation| a.channels[0].sampler.times = vec![0.0, 0.5, 0.5],
        &|a: &mut Animation| a.channels[0].sampler.times = vec![],
        &|a: &mut Animation| {
            a.channels[1].sampler.values.pop();
        },
        &|a: &mut Animation| a.channels.push(a.channels[1].clone()),
    ] as [&dyn Fn(&mut Animation); 5]
    {
        assert!(matches!(refuse(change), IoError::Malformed { .. }));
    }
}

#[test]
fn cubic_spline_reads_as_stored_and_unknown_paths_are_malformed() {
    let mut a = clip();
    a.channels.truncate(1);
    let bytes = write(&a).unwrap();
    let parsed = omuzaki_gltf::parse(&bytes, &Limits::default()).unwrap();
    let mut doc = parsed.document.clone();
    let chunk = parsed.chunk.unwrap().to_vec();
    // As CUBICSPLINE, the nine floats of the translation output are one key's in-tangent, value and out-tangent.
    doc.animations[0]["samplers"][0]["interpolation"] = "CUBICSPLINE".into();
    doc.accessors[0].count = 1;
    let mut loader = Loader::new(&NoResolver, u64::MAX);
    let buffers = Buffers::load(&doc, Some(&chunk), &mut loader).unwrap();
    let back = read_animations(&doc, &Accessors::new(&doc, &buffers)).unwrap();
    let s = &back[0].channels[0].sampler;
    assert_eq!((s.interpolation, s.times.len(), s.values.len()), (Interpolation::CubicSpline, 1, 9));

    doc.animations[0]["channels"][0]["target"]["path"] = "colour".into();
    assert!(matches!(read_animations(&doc, &Accessors::new(&doc, &buffers)), Err(IoError::Malformed { .. })));
}

#[test]
fn counts_are_checked_before_anything_is_allocated() {
    let bytes = write(&clip()).unwrap();
    let parsed = omuzaki_gltf::parse(&bytes, &Limits::default()).unwrap();
    let chunk = parsed.chunk.unwrap().to_vec();
    let mut doc = parsed.document.clone();
    doc.accessors[0].count = MAX_KEYS + 1;
    let mut loader = Loader::new(&NoResolver, u64::MAX);
    let buffers = Buffers::load(&doc, Some(&chunk), &mut loader).unwrap();
    let err = read_animations(&doc, &Accessors::new(&doc, &buffers)).unwrap_err();
    assert!(matches!(err, IoError::Limit { what: "animation key count", .. }), "{err:?}");

    let mut doc = parsed.document.clone();
    doc.animations[0]["channels"] = serde_json::Value::Array(vec![serde_json::json!({}); 70_000]);
    let err = read_animations(&doc, &Accessors::new(&doc, &buffers)).unwrap_err();
    assert!(matches!(err, IoError::Limit { what: "animation channel count", .. }), "{err:?}");
}
