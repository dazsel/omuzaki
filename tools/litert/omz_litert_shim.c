/* The four functions Omuzaki adds to its LiteRT library (ADR 0061 decision 2; docs/design/litert-runtime.md §3.3).
 *
 * `TfLiteXNNPackDelegateOptions` grows between LiteRT releases, and LiteRT's error reporter takes a C `va_list`, so
 * Rust sees neither: this file is compiled against the pinned headers, and the Rust side only calls these functions.
 * Change a signature here and bump OMZ_LITERT_ABI, which `omuzaki-ai-litert` checks when it loads the library.
 */
#include <stdarg.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>

#include "tflite/c/c_api.h"
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

/* What the reporter needs. LiteRT hands the reporter one pointer, so this lives in the last bytes of the caller's
 * buffer, after the text it may fill. */
typedef struct {
  char* text;
  size_t text_cap;
} OmzErrorSink;

/* Writes one LiteRT message over the previous one, truncated and NUL-terminated within the text's room. */
static void omz_litert_report(void* user_data, const char* format, va_list args) {
  OmzErrorSink* sink = (OmzErrorSink*)user_data;
  vsnprintf(sink->text, sink->text_cap, format, args);
}

/* Routes the error messages of interpreters created from `options` into `buf`, which must stay in place and must not
 * be written by the caller while any of them lives. The reporter's state takes the last `sizeof(OmzErrorSink)` bytes,
 * plus at most its alignment less one; the text before it is always NUL-terminated, so a reader that stops at the
 * first NUL never reaches the state. `buf` starts empty. A NULL argument, or a `cap` too small for the state and one
 * byte of text, installs nothing. */
TFL_CAPI_EXPORT void omz_litert_capture_errors(TfLiteInterpreterOptions* options, char* buf, size_t cap) {
  const size_t align = _Alignof(OmzErrorSink);
  if (!options || !buf || cap < sizeof(OmzErrorSink) + align + 1) return;
  const uintptr_t sink_at = ((uintptr_t)buf + cap - sizeof(OmzErrorSink)) & ~(uintptr_t)(align - 1);
  OmzErrorSink* sink = (OmzErrorSink*)sink_at;
  sink->text = buf;
  sink->text_cap = (size_t)(sink_at - (uintptr_t)buf);
  buf[0] = 0;
  TfLiteInterpreterOptionsSetErrorReporter(options, omz_litert_report, sink);
}
