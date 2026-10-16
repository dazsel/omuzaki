//! The file-access path parser (`omuzaki-ops-api/src/file_access/form.rs`) on arbitrary text, in both grammars: no
//! panic, and no broken promise (`fuzzing::check_path`).
#![no_main]

use libfuzzer_sys::fuzz_target;
use omuzaki_ops_api::fuzzing::check_path;

fuzz_target!(|data: &[u8]| {
    if let Ok(raw) = std::str::from_utf8(data)
        && let Err(broken) = check_path(raw)
    {
        panic!("{broken}");
    }
});
