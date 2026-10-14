// A flat C interface over the Spout2 SDK's spoutDX class, for Rust FFI. Only what the spike needs.
// All functions are safe to call with a null handle (they report failure).
#pragma once
#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct BoSender BoSender;
typedef struct BoReceiver BoReceiver;

// What a receive poll learned. `connected` is 0 when no sender with that name exists.
typedef struct BoReceiveInfo {
    uint32_t connected;
    uint32_t updated;      // the sender appeared or changed size/format: poll again before reading
    uint32_t new_frame;    // Spout's own verdict; always 1 unless the per-user "Framecount" registry setting is on, so do not trust it
    uint32_t width;
    uint32_t height;
    uint32_t dxgi_format;
    uint64_t sender_frame; // the sender's frame counter (0 unless the Framecount registry setting is on)
    double sender_fps;
} BoReceiveInfo;

// Sender. `d3d11_device` is an ID3D11Device* (for the D3D12 route: the D3D11On12 device), or null to let Spout
// create its own. The name is what receivers select; Spout appends _1, _2 ... if it is taken.
BoSender* bo_sender_new(void* d3d11_device, const char* name);
void bo_sender_free(BoSender* sender);
// `texture` is an ID3D11Texture2D* on the sender's device. Spout copies it into its shared texture and flushes.
int bo_sender_send_texture(BoSender* sender, void* texture);
// CPU path: tightly packed BGRA8 rows (pitch 0) or `pitch` bytes per row.
int bo_sender_send_image(BoSender* sender, const uint8_t* pixels, uint32_t width, uint32_t height, uint32_t pitch);
// Blocks until every command the sender's D3D11 device has been given (including the copy into Spout's shared texture)
// has finished on the GPU, using a D3D11 event query. Returns 1 when done, 0 on timeout (one second) or error.
int bo_sender_wait_gpu(BoSender* sender);
// Signals the named sync event a receiver may be waiting on (call after each send).
void bo_sender_signal_sync(BoSender* sender);
// The registered sender name (may differ from the requested one). Null before the first send.
const char* bo_sender_name(const BoSender* sender);

// Receiver. A null or empty name connects to the active sender.
BoReceiver* bo_receiver_new(const char* name);
void bo_receiver_free(BoReceiver* receiver);
// Waits up to `timeout_ms` for the sender's sync event; returns 1 when signalled.
int bo_receiver_wait_sync(BoReceiver* receiver, uint32_t timeout_ms);
// Connects, and copies the sender's newest frame into the receiver's own texture when there is one.
int bo_receiver_poll(BoReceiver* receiver, BoReceiveInfo* info);
// Reads the texture the last poll received into `dst` (tightly packed rows, width*4 bytes each). Returns 1 on success.
int bo_receiver_read_pixels(BoReceiver* receiver, uint8_t* dst, size_t dst_len);

#ifdef __cplusplus
}
#endif
