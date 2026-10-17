//! Lets the test executables export the symbols they define, so the `dlopen`ed ONNX Runtime binds to the native
//! allocation counter in `omuzaki-ai-ort/tests/common/native_alloc.rs` (see its docs). Linux only; elsewhere the test
//! says it cannot count native allocations.

fn main() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("linux") {
        println!("cargo::rustc-link-arg-tests=-Wl,--export-dynamic");
    }
}
