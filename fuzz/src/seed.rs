//! Writes the seed corpora (#1653): `seed <folder>` fills `<folder>/<target>/` for each fuzz target.
//!
//! - `litert_scan`: the scan tests' synthetic FlatBuffers, built with their own builder (`tests/common/model.rs`):
//!   the accepted `ADD` graph and one variant per kind of refusal, so the fuzzer starts deep in each table.
//! - `ort_scan`: the `onnx` runtime's synthetic test models (`tests/fixtures/*.onnx`, a few hundred bytes each, made by
//!   their `make_models.py`).
//! - `file_access_path`: the path forms `form.rs`'s tests use, in both grammars.
//!
//! Nothing is downloaded, and the corpora are not committed (`.gitignore`).

#[allow(dead_code, clippy::all)]
#[path = "../../crates/omuzaki-ai-litert/tests/common/model.rs"]
mod model;

use std::path::Path;

use model::{Table, Value, add_graph, builtin, bytes, operator_of, slot, tensor_of};

/// The scan tests' models: accepted, dynamic, and one per refusal.
fn tflite_seeds() -> Vec<(&'static str, Vec<u8>)> {
    let with = |change: &dyn Fn(&mut Table)| {
        let mut model = add_graph();
        change(&mut model);
        bytes(&model)
    };
    vec![
        ("add", bytes(&add_graph())),
        (
            "dynamic",
            with(&|m| {
                _ = tensor_of(m, 0).set(0, Value::Ints(vec![1, 1, 1, 3])).set(7, Value::Ints(vec![1, -1, -1, 3]))
            }),
        ),
        ("custom", with(&|m| _ = m.at(slot::CODES, 0).set(1, Value::Str("FlexAddV2")))),
        ("stateful", with(&|m| *m.at(slot::CODES, 0) = builtin(146))),
        ("variable", with(&|m| _ = tensor_of(m, 2).set(5, Value::Bool(true)))),
        ("mutating", with(&|m| _ = operator_of(m).set(7, Value::Bools(vec![false, true])))),
        ("options2", with(&|m| _ = operator_of(m).set(11, Value::I8(1)))),
        (
            "external",
            with(&|m| {
                _ = m.set(slot::BUFFERS, Value::Tables(vec![Table(vec![(1, Value::U64(64)), (2, Value::U64(8))])]))
            }),
        ),
        ("quantized", with(&|m| _ = tensor_of(m, 2).set(4, Value::Table(Table(vec![(4, Value::I8(1))]))))),
        (
            "metadata",
            with(&|m| {
                _ = m.set(
                    6,
                    Value::Tables(vec![Table(vec![(0, Value::Str("min_runtime_version")), (1, Value::U32(0))])]),
                )
            }),
        ),
        ("newer", with(&|m| _ = tensor_of(m, 0).set(11, Value::U32(1)))),
    ]
}

/// The `onnx` runtime's synthetic test models.
const ONNX_SEEDS: &[(&str, &[u8])] = &[
    ("add", include_bytes!("../../crates/omuzaki-ai-ort/tests/fixtures/add.onnx")),
    ("add_sub", include_bytes!("../../crates/omuzaki-ai-ort/tests/fixtures/add_sub.onnx")),
    ("custom_op", include_bytes!("../../crates/omuzaki-ai-ort/tests/fixtures/custom_op.onnx")),
    ("external_data", include_bytes!("../../crates/omuzaki-ai-ort/tests/fixtures/external_data.onnx")),
    ("identity", include_bytes!("../../crates/omuzaki-ai-ort/tests/fixtures/identity.onnx")),
    ("long_names", include_bytes!("../../crates/omuzaki-ai-ort/tests/fixtures/long_names.onnx")),
    ("many", include_bytes!("../../crates/omuzaki-ai-ort/tests/fixtures/many.onnx")),
    ("stage_nine", include_bytes!("../../crates/omuzaki-ai-ort/tests/fixtures/stage_nine.onnx")),
];

/// The forms `form.rs`'s tests read: POSIX, drive, UNC, verbatim, device, reserved names, streams, `..`.
const PATH_SEEDS: &[&str] = &[
    "/home/u/art/sub//./x.png",
    "/a/b/../c/./d",
    "/a/../..",
    "/..",
    "relative/x",
    "../../x",
    ".hidden",
    "/a:b/c\\d",
    r"C:\Users\U\Art",
    "C:/x",
    "c:x",
    "C:",
    r"C:\..",
    r"C:\a\.\b",
    r"\x",
    r"\\server\share\f",
    r"//Srv/Share/a/b",
    r"\\?\C:\Users\u\a.txt",
    r"\\?\UNC\srv\share\a\b",
    r"\\?\C:\a\..\b",
    r"\\?\C:\a/b",
    r"\\?\GLOBALROOT\Device\x",
    r"\\.\pipe\x",
    "//?/C:/x",
    "CON",
    "nul.txt",
    "COM1",
    "COM\u{b9}",
    "lpt9.log",
    "file.txt::$DATA",
    "trailing.",
    "trailing ",
    "a|b",
    r"C:\Küche\x",
    "\u{212a}",
    "PROGRA~1",
];

fn write_all(folder: &Path, items: impl IntoIterator<Item = (String, Vec<u8>)>) -> std::io::Result<usize> {
    std::fs::create_dir_all(folder)?;
    let mut count = 0;
    for (name, data) in items {
        std::fs::write(folder.join(name), data)?;
        count += 1;
    }
    Ok(count)
}

fn main() -> std::io::Result<()> {
    let Some(root) = std::env::args_os().nth(1) else {
        eprintln!("usage: seed <corpus folder>");
        std::process::exit(2);
    };
    let root = Path::new(&root);
    let tflite =
        write_all(&root.join("litert_scan"), tflite_seeds().into_iter().map(|(n, d)| (format!("{n}.tflite"), d)))?;
    let onnx = write_all(&root.join("ort_scan"), ONNX_SEEDS.iter().map(|(n, d)| (format!("{n}.onnx"), d.to_vec())))?;
    let paths = write_all(
        &root.join("file_access_path"),
        PATH_SEEDS.iter().enumerate().map(|(i, p)| (format!("path{i:02}"), p.as_bytes().to_vec())),
    )?;
    println!("seeded {tflite} TFLite models, {onnx} ONNX models, {paths} paths under {}", root.display());
    Ok(())
}
