"""Writes the tiny ONNX models the omuzaki-ai-ort tests use, byte for byte reproducibly.

    python3 make_models.py [out_dir]      # default: the folder this script is in

Needs `onnx` (Apache-2.0; dev-only, never shipped). The models are checked in, so the tests don't run this.
  identity.onnx       x f32[1,4] -> y f32[1,4], one Identity node (opset 13)
  add.onnx            a, b f32[batch,3] -> c, one Add node: a dynamic first dimension
  add_sub.onnx        a, b f32[batch,3] -> c = a + b, d = a - b: two outputs, for the "nothing is written on a rejection" test
  many.onnx           7 inputs x0..x6 f32[2] -> 5 outputs: more than ort keeps on the stack (6 inputs, 4 outputs)
  long_names.onnx     one 800-byte input name: ort's per-run name arrays copy to the heap at 768 bytes or more
  custom_op.onnx      a node in the domain `com.example.custom`: refused (custom-op domain)
  external_data.onnx  an initializer stored in a file beside the model: refused (external data)
  stage_a.onnx        x f32[1,4] -> y = x + x: the doubling stage of the pipeline tests
  stage_b.onnx        p, q f32[1,4] -> z = p + q: the adding stage
  stage_cast.onnx     x f32[1,4] -> y int64[1,4], one Cast node: a stage whose output type differs
"""
import sys
from pathlib import Path

import onnx
from onnx import TensorProto, helper

OPSET = 13
IR_VERSION = 8  # fixed, so the bytes don't change with the installed onnx


def save(model, out_dir, name):
    onnx.checker.check_model(model)
    model.ir_version = IR_VERSION
    (out_dir / name).write_bytes(model.SerializeToString(deterministic=True))


def vi(name, dims):
    return helper.make_tensor_value_info(name, TensorProto.FLOAT, dims)


def model(graph, extra_opsets=()):
    opsets = [helper.make_opsetid("", OPSET), *extra_opsets]
    return helper.make_model(graph, opset_imports=opsets, producer_name="ai-ort-fixtures")


def main():
    out = Path(sys.argv[1]) if len(sys.argv) > 1 else Path(__file__).parent
    out.mkdir(parents=True, exist_ok=True)

    ident = helper.make_graph([helper.make_node("Identity", ["x"], ["y"])], "identity", [vi("x", [1, 4])], [vi("y", [1, 4])])
    save(model(ident), out, "identity.onnx")

    add = helper.make_graph(
        [helper.make_node("Add", ["a", "b"], ["c"])], "add",
        [vi("a", ["batch", 3]), vi("b", ["batch", 3])], [vi("c", ["batch", 3])],
    )
    save(model(add), out, "add.onnx")

    add_sub = helper.make_graph(
        [helper.make_node("Add", ["a", "b"], ["c"]), helper.make_node("Sub", ["a", "b"], ["d"])], "add_sub",
        [vi("a", ["batch", 3]), vi("b", ["batch", 3])], [vi("c", ["batch", 3]), vi("d", ["batch", 3])],
    )
    save(model(add_sub), out, "add_sub.onnx")

    many = helper.make_graph(
        [
            helper.make_node("Identity", ["x0"], ["o0"]),
            helper.make_node("Identity", ["x1"], ["o1"]),
            helper.make_node("Identity", ["x2"], ["o2"]),
            helper.make_node("Add", ["x3", "x4"], ["o3"]),
            helper.make_node("Add", ["x5", "x6"], ["o4"]),
        ],
        "many",
        [vi(f"x{i}", [2]) for i in range(7)],
        [vi(f"o{i}", [2]) for i in range(5)],
    )
    save(model(many), out, "many.onnx")

    long = "x" * 800
    long_names = helper.make_graph([helper.make_node("Identity", [long], ["y"])], "long", [vi(long, [1, 4])], [vi("y", [1, 4])])
    save(model(long_names), out, "long_names.onnx")

    custom = helper.make_graph(
        [helper.make_node("Foo", ["x"], ["y"], domain="com.example.custom")], "custom", [vi("x", [1, 4])], [vi("y", [1, 4])]
    )
    save(model(custom, [helper.make_opsetid("com.example.custom", 1)]), out, "custom_op.onnx")

    weights = TensorProto()
    weights.name = "w"
    weights.data_type = TensorProto.FLOAT
    weights.dims.extend([1, 4])
    weights.data_location = TensorProto.EXTERNAL
    for key, value in (("location", "weights.bin"), ("offset", "0"), ("length", "16")):
        entry = weights.external_data.add()
        entry.key, entry.value = key, value
    ext = helper.make_graph(
        [helper.make_node("Add", ["x", "w"], ["y"])], "external", [vi("x", [1, 4])], [vi("y", [1, 4])], initializer=[weights]
    )
    ext_model = model(ext)
    ext_model.ir_version = IR_VERSION
    (out / "external_data.onnx").write_bytes(ext_model.SerializeToString(deterministic=True))  # the checker needs the file

    stage_a = helper.make_graph([helper.make_node("Add", ["x", "x"], ["y"])], "stage_a", [vi("x", [1, 4])], [vi("y", [1, 4])])
    save(model(stage_a), out, "stage_a.onnx")

    stage_b = helper.make_graph(
        [helper.make_node("Add", ["p", "q"], ["z"])], "stage_b", [vi("p", [1, 4]), vi("q", [1, 4])], [vi("z", [1, 4])]
    )
    save(model(stage_b), out, "stage_b.onnx")

    cast = helper.make_graph(
        [helper.make_node("Cast", ["x"], ["y"], to=TensorProto.INT64)], "stage_cast", [vi("x", [1, 4])],
        [helper.make_tensor_value_info("y", TensorProto.INT64, [1, 4])],
    )
    save(model(cast), out, "stage_cast.onnx")


if __name__ == "__main__":
    main()
