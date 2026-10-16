//! The `onnx` runtime's model scan (`omuzaki-ai-ort/src/scan.rs`) on arbitrary bytes: an error or a scan, never a
//! panic, and the same answer twice.
#![no_main]

use libfuzzer_sys::fuzz_target;
use omuzaki_ai_ort::scan_model;

fuzz_target!(|data: &[u8]| {
    assert_eq!(scan_model(data), scan_model(data));
});
