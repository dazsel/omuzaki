/* The smoke program the LiteRT workflow's check job runs against each built library (design §8.2).
 *
 *   omz_litert_smoke LIBRARY SYMBOLS
 *       loads LIBRARY the way omuzaki-ai-litert does (at run time, nothing linked), resolves every name in the
 *       SYMBOLS file (symbols.txt), and prints `version <TfLiteVersion()>` and `abi <omz_litert_abi()>`.
 *   omz_litert_smoke LIBRARY SYMBOLS MODEL OUTPUT INPUT...
 *       also runs MODEL once on one thread through the shim's XNNPACK delegate: each INPUT file holds the raw bytes
 *       of one input tensor, in order, and OUTPUT receives every output tensor's bytes, in order. Then it resizes the
 *       first input to a shape no face graph takes and prints `error <text>`: the message omz_litert_capture_errors
 *       caught when AllocateTensors refused it.
 *
 * Exits non-zero, naming the problem, on any failure. Built from the LiteRT tree by Bazel (BUILD.omuzaki).
 */
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#ifdef _WIN32
#include <windows.h>
typedef HMODULE library_t;
static library_t open_library(const char* path) { return LoadLibraryA(path); }
static void* find_symbol(library_t lib, const char* name) { return (void*)GetProcAddress(lib, name); }
#else
#include <dlfcn.h>
typedef void* library_t;
static library_t open_library(const char* path) { return dlopen(path, RTLD_NOW | RTLD_LOCAL); }
static void* find_symbol(library_t lib, const char* name) { return dlsym(lib, name); }
#endif

/* Opaque handles: the program only passes them back to the library. */
typedef struct Opaque Opaque;

/* The C API signatures the program calls, as in tflite/core/c/c_api.h at the pinned tag (`TfLiteStatus` is an int
 * enum; the tensor type and delegate are opaque here). */
typedef const char* (*version_fn)(void);
typedef uint32_t (*abi_fn)(void);
typedef Opaque* (*model_create_fn)(const void*, size_t);
typedef void (*delete_fn)(Opaque*);
typedef Opaque* (*options_create_fn)(void);
typedef void (*set_threads_fn)(Opaque*, int32_t);
typedef void (*add_delegate_fn)(Opaque*, Opaque*);
typedef Opaque* (*interpreter_create_fn)(const Opaque*, const Opaque*);
typedef int (*status_fn)(Opaque*);
typedef int32_t (*count_fn)(const Opaque*);
typedef Opaque* (*input_fn)(const Opaque*, int32_t);
typedef const Opaque* (*output_fn)(const Opaque*, int32_t);
typedef size_t (*byte_size_fn)(const Opaque*);
typedef int (*copy_from_fn)(Opaque*, const void*, size_t);
typedef int (*copy_to_fn)(const Opaque*, void*, size_t);
typedef Opaque* (*xnnpack_create_fn)(int32_t);
typedef void (*capture_errors_fn)(Opaque*, char*, size_t);
typedef int (*resize_fn)(Opaque*, int32_t, const int*, int32_t);

static library_t library;
/* Where omz_litert_capture_errors writes LiteRT's messages; the size the Rust session uses (ERROR_BYTES). */
static char errors[1024];

static void fail(const char* what, const char* detail) {
  fprintf(stderr, "omz_litert_smoke: %s%s%s\n", what, detail ? ": " : "", detail ? detail : "");
  exit(1);
}

static void* need(const char* name) {
  void* symbol = find_symbol(library, name);
  if (!symbol) fail("symbol not exported", name);
  return symbol;
}

/* Resolves every name listed in the symbols file; `#` lines and blank lines are skipped. */
static void check_symbols(const char* path) {
  FILE* file = fopen(path, "r");
  if (!file) fail("cannot open", path);
  char line[256];
  int resolved = 0;
  while (fgets(line, sizeof line, file)) {
    line[strcspn(line, "\r\n")] = 0;
    if (line[0] == 0 || line[0] == '#') continue;
    need(line);
    resolved++;
  }
  fclose(file);
  printf("symbols %d\n", resolved);
}

/* Reads a whole file into a buffer the caller frees. */
static void* read_file(const char* path, size_t* size) {
  FILE* file = fopen(path, "rb");
  if (!file) fail("cannot open", path);
  if (fseek(file, 0, SEEK_END) != 0) fail("cannot seek", path);
  long length = ftell(file);
  if (length < 0 || fseek(file, 0, SEEK_SET) != 0) fail("cannot size", path);
  void* data = malloc(length > 0 ? (size_t)length : 1);
  if (!data) fail("out of memory reading", path);
  if (fread(data, 1, (size_t)length, file) != (size_t)length) fail("cannot read", path);
  fclose(file);
  *size = (size_t)length;
  return data;
}

static void run_model(const char* model_path, const char* output_path, int input_count, char** input_paths) {
  size_t model_size;
  void* model_bytes = read_file(model_path, &model_size);
  Opaque* model = ((model_create_fn)need("TfLiteModelCreate"))(model_bytes, model_size);
  if (!model) fail("TfLiteModelCreate refused", model_path);
  Opaque* delegate = ((xnnpack_create_fn)need("omz_litert_xnnpack_create"))(1);
  if (!delegate) fail("omz_litert_xnnpack_create returned NULL", NULL);
  Opaque* options = ((options_create_fn)need("TfLiteInterpreterOptionsCreate"))();
  ((set_threads_fn)need("TfLiteInterpreterOptionsSetNumThreads"))(options, 1);
  ((add_delegate_fn)need("TfLiteInterpreterOptionsAddDelegate"))(options, delegate);
  ((capture_errors_fn)need("omz_litert_capture_errors"))(options, errors, sizeof errors);
  Opaque* interpreter = ((interpreter_create_fn)need("TfLiteInterpreterCreate"))(model, options);
  if (!interpreter) fail("TfLiteInterpreterCreate failed", model_path);
  if (((status_fn)need("TfLiteInterpreterAllocateTensors"))(interpreter) != 0) fail("AllocateTensors failed", NULL);

  if (((count_fn)need("TfLiteInterpreterGetInputTensorCount"))(interpreter) != input_count) {
    fail("input count differs from the model's", model_path);
  }
  for (int i = 0; i < input_count; i++) {
    size_t size;
    void* data = read_file(input_paths[i], &size);
    Opaque* tensor = ((input_fn)need("TfLiteInterpreterGetInputTensor"))(interpreter, i);
    if (((byte_size_fn)need("TfLiteTensorByteSize"))(tensor) != size) fail("input size differs", input_paths[i]);
    if (((copy_from_fn)need("TfLiteTensorCopyFromBuffer"))(tensor, data, size) != 0) fail("copy in failed", NULL);
    free(data);
  }
  if (((status_fn)need("TfLiteInterpreterInvoke"))(interpreter) != 0) fail("Invoke failed", model_path);

  FILE* out = fopen(output_path, "wb");
  if (!out) fail("cannot create", output_path);
  int32_t outputs = ((count_fn)need("TfLiteInterpreterGetOutputTensorCount"))(interpreter);
  for (int32_t i = 0; i < outputs; i++) {
    const Opaque* tensor = ((output_fn)need("TfLiteInterpreterGetOutputTensor"))(interpreter, i);
    size_t size = ((byte_size_fn)need("TfLiteTensorByteSize"))(tensor);
    void* data = malloc(size > 0 ? size : 1);
    if (!data) fail("out of memory", NULL);
    if (((copy_to_fn)need("TfLiteTensorCopyToBuffer"))(tensor, data, size) != 0) fail("copy out failed", NULL);
    if (fwrite(data, 1, size, out) != size) fail("cannot write", output_path);
    free(data);
  }
  if (fclose(out) != 0) fail("cannot write", output_path);
  printf("outputs %d\n", (int)outputs);

  /* A rank-1 input of one element fits none of the face graphs, so AllocateTensors must fail and say why. */
  const int bad_shape[1] = {1};
  if (((resize_fn)need("TfLiteInterpreterResizeInputTensor"))(interpreter, 0, bad_shape, 1) != 0) {
    fail("ResizeInputTensor refused the shape", errors);
  }
  if (((status_fn)need("TfLiteInterpreterAllocateTensors"))(interpreter) == 0) fail("a bad shape was allocated", NULL);
  if (errors[0] == 0) fail("AllocateTensors failed but no error text was captured", NULL);
  errors[strcspn(errors, "\r\n")] = 0;
  printf("error %s\n", errors);

  /* The interpreter goes before the delegate it uses (design §3.5). */
  ((delete_fn)need("TfLiteInterpreterDelete"))(interpreter);
  ((delete_fn)need("omz_litert_xnnpack_delete"))(delegate);
  ((delete_fn)need("TfLiteInterpreterOptionsDelete"))(options);
  ((delete_fn)need("TfLiteModelDelete"))(model);
  free(model_bytes);
}

int main(int argc, char** argv) {
  if (argc != 3 && argc < 6) {
    fprintf(stderr, "usage: %s LIBRARY SYMBOLS [MODEL OUTPUT INPUT...]\n", argv[0]);
    return 2;
  }
  library = open_library(argv[1]);
  if (!library) fail("cannot load", argv[1]);
  check_symbols(argv[2]);
  printf("version %s\n", ((version_fn)need("TfLiteVersion"))());
  printf("abi %u\n", (unsigned)((abi_fn)need("omz_litert_abi"))());
  if (argc >= 6) run_model(argv[3], argv[4], argc - 5, argv + 5);
  return 0;
}
