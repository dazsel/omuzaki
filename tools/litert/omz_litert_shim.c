/* The three functions Omuzaki adds to its LiteRT library (ADR 0061 decision 2; docs/design/litert-runtime.md §3.3).
 *
 * `TfLiteXNNPackDelegateOptions` grows between LiteRT releases, so Rust never sees it: this file is compiled against
 * the pinned header, and the Rust side only calls these functions. Change a signature here and bump OMZ_LITERT_ABI,
 * which `omuzaki-ai-litert` checks when it loads the library.
 */
#include <stdint.h>

#include "tflite/c/common.h"
#include "tflite/delegates/xnnpack/xnnpack_delegate.h"
#include "xnnpack.h"

/* Bumped whenever a signature below changes. */
#define OMZ_LITERT_ABI 1u

/* Returns OMZ_LITERT_ABI, so a loader can refuse a library built from another version of this file. */
TFL_CAPI_EXPORT uint32_t omz_litert_abi(void) { return OMZ_LITERT_ABI; }

/* Creates an XNNPACK delegate with Omuzaki's options: no thread pool at one thread (the work runs on the caller's
 * thread), workers that yield instead of spinning, FP32 arithmetic, and no weight cache (nothing is shared between
 * interpreters and no cache file may be written). Returns NULL when XNNPACK cannot start. */
TFL_CAPI_EXPORT TfLiteDelegate* omz_litert_xnnpack_create(int32_t threads) {
  TfLiteXNNPackDelegateOptions options = TfLiteXNNPackDelegateOptionsDefault();
  options.num_threads = threads > 1 ? threads : 0;
  options.runtime_flags = XNN_FLAG_DONT_SPIN_WORKERS;
  /* FP16 changes results beyond the parity tolerance (design §7). */
  options.flags &= ~(uint32_t)TFLITE_XNNPACK_DELEGATE_FLAG_FORCE_FP16;
  options.weights_cache = NULL;
  options.weight_cache_file_path = NULL;
  options.weight_cache_file_descriptor = -1;
  options.weight_cache_provider = NULL;
  return TfLiteXNNPackDelegateCreate(&options);
}

/* Deletes a delegate from omz_litert_xnnpack_create, after every interpreter using it is gone. NULL is ignored. */
TFL_CAPI_EXPORT void omz_litert_xnnpack_delete(TfLiteDelegate* delegate) { TfLiteXNNPackDelegateDelete(delegate); }
