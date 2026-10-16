//! The `tflite` runtime's model scan (`omuzaki-ai-litert/src/scan.rs`) on arbitrary bytes: an error or a scan, never a
//! panic, and an accepted model's ports within the scan's own caps.
#![no_main]

use libfuzzer_sys::fuzz_target;
use omuzaki_ai_api::MAX_RANK;
use omuzaki_ai_litert::scan::{MAX_PORTS, scan_model};

fuzz_target!(|data: &[u8]| {
    if let Ok(scan) = scan_model(data) {
        let signature = scan.signature;
        assert!((1..=MAX_PORTS).contains(&signature.inputs.len()));
        assert!((1..=MAX_PORTS).contains(&signature.outputs.len()));
        assert!(signature.inputs.iter().chain(&signature.outputs).all(|spec| spec.shape.len() <= MAX_RANK));
    }
});
