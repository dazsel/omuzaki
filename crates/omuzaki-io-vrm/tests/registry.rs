//! Registration: both generations register an importer and an exporter, and the registry's sniffing picks the right one.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use omuzaki_io_api::{
    Confidence, Detection, Exporter, FormatId, ImportInput, Importer, find_exporter, find_importer, formats, sniff,
};
use omuzaki_io_vrm::{Vrm0Exporter, Vrm1Exporter, Vrm1Importer};
use serde_json::json;

use common::{avatar, export, glb, minimal0, vrm0_friendly};

/// Linking is what registers the formats, and a linker drops a crate nothing refers to, so touch it.
fn linked() {
    let _ = (Vrm1Importer.info(), Vrm1Exporter.info(), Vrm0Exporter.info());
}

#[test]
fn both_generations_register_an_importer_and_an_exporter() {
    linked();
    let all = formats();
    for id in ["vrm0", "vrm1"] {
        let f = all.iter().find(|f| f.info.id == id).unwrap_or_else(|| panic!("{id} is not registered"));
        assert!(f.import && f.export, "{id}");
        assert_eq!(f.info.extensions, ["vrm"]);
        assert_eq!(f.info.media_type, "model/gltf-binary");
        assert!(id.parse::<FormatId>().is_ok());
        assert_eq!(find_importer(id).unwrap().info().id, id);
        assert_eq!(find_exporter(id).unwrap().info().id, id);
    }
}

#[test]
fn the_registry_picks_the_importer_that_recognises_the_file() {
    linked();
    let one = export(&Vrm1Exporter, &avatar().0);
    assert_eq!(sniff(&ImportInput::new(&one)), [Detection { format: "vrm1", confidence: Confidence::Certain }]);
    let zero = export(&Vrm0Exporter, &vrm0_friendly(avatar()).0);
    assert_eq!(sniff(&ImportInput::new(&zero)), [Detection { format: "vrm0", confidence: Confidence::Certain }]);
    let (doc, bin) = minimal0(&json!({}));
    assert_eq!(sniff(&ImportInput::new(&glb(&doc, &bin)))[0].format, "vrm0");
    // A plain glTF is nobody's VRM.
    let plain = glb(&json!({"asset": {"version": "2.0"}}), &[]);
    assert_eq!(sniff(&ImportInput::new(&plain)), []);
}
