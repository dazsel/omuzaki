// Counts the nodes of a model that Omuzaki's XNNPACK delegate takes (the check job's delegated-node count).
//
//   omz_litert_plan MODEL
//       prints `delegated <nodes taken> of <nodes> in <partitions> partitions`
//
// The C API the library exports can't see an interpreter's execution plan, and LiteRT logs its own count only in
// debug builds, so this program links the same LiteRT sources and the shim statically, built in the same Bazel run
// with the same flags as the library, and applies the delegate exactly as the library's C API does.
#include <cstdio>
#include <memory>

#include "tflite/core/c/common.h"
#include "tflite/interpreter.h"
#include "tflite/interpreter_builder.h"
#include "tflite/kernels/register.h"
#include "tflite/model_builder.h"

extern "C" TfLiteDelegate* omz_litert_xnnpack_create(int32_t threads);
extern "C" void omz_litert_xnnpack_delete(TfLiteDelegate* delegate);

namespace {

int Fail(const char* what) {
  std::fprintf(stderr, "omz_litert_plan: %s\n", what);
  return 1;
}

}  // namespace

int main(int argc, char** argv) {
  if (argc != 2) return Fail("usage: omz_litert_plan MODEL");
  std::unique_ptr<tflite::FlatBufferModel> model = tflite::FlatBufferModel::BuildFromFile(argv[1]);
  if (!model) return Fail("cannot read the model");
  // Without the default delegates, so the only delegate applied is ours.
  tflite::ops::builtin::BuiltinOpResolverWithoutDefaultDelegates resolver;
  std::unique_ptr<tflite::Interpreter> interpreter;
  if (tflite::InterpreterBuilder(*model, resolver)(&interpreter) != kTfLiteOk) return Fail("cannot build");
  const size_t nodes = interpreter->execution_plan().size();

  std::unique_ptr<TfLiteDelegate, void (*)(TfLiteDelegate*)> delegate(omz_litert_xnnpack_create(1),
                                                                       omz_litert_xnnpack_delete);
  if (!delegate) return Fail("omz_litert_xnnpack_create returned NULL");
  if (interpreter->ModifyGraphWithDelegate(delegate.get()) != kTfLiteOk) return Fail("cannot apply the delegate");

  // Each partition the delegate took is one node in the plan, carrying the original nodes it replaced.
  int taken = 0;
  int partitions = 0;
  for (int index : interpreter->execution_plan()) {
    const TfLiteNode& node = interpreter->node_and_registration(index)->first;
    if (node.delegate != delegate.get()) continue;
    partitions++;
    taken += static_cast<const TfLiteDelegateParams*>(node.builtin_data)->nodes_to_replace->size;
  }
  std::printf("delegated %d of %zu in %d partitions\n", taken, nodes, partitions);
  interpreter.reset();  // before the delegate it uses
  return 0;
}
