// See bo_shim.h. Thin on purpose: the Spout SDK does the protocol work (sender registry, shared texture, mutex, sync).
#include "bo_shim.h"

#include <new>
#include <string>

#include "SpoutDX.h"

struct BoSender {
    spoutDX dx;
    ID3D11Query* gpu_done = nullptr;  // event query for bo_sender_wait_gpu

    ~BoSender() {
        if (gpu_done) gpu_done->Release();
    }
};

struct BoReceiver {
    spoutDX dx;
    std::string wanted;
    ID3D11Texture2D* staging = nullptr;
    UINT staging_w = 0;
    UINT staging_h = 0;
    DXGI_FORMAT staging_format = DXGI_FORMAT_UNKNOWN;

    ~BoReceiver() { release_staging(); }

    void release_staging() {
        if (staging) {
            staging->Release();
            staging = nullptr;
        }
    }

    // A CPU-readable copy target matching the received texture; rebuilt when the sender changes size or format.
    bool ensure_staging(const D3D11_TEXTURE2D_DESC& src) {
        if (staging && staging_w == src.Width && staging_h == src.Height && staging_format == src.Format) return true;
        release_staging();
        D3D11_TEXTURE2D_DESC desc = {};
        desc.Width = src.Width;
        desc.Height = src.Height;
        desc.MipLevels = 1;
        desc.ArraySize = 1;
        desc.Format = src.Format;
        desc.SampleDesc.Count = 1;
        desc.Usage = D3D11_USAGE_STAGING;
        desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ;
        if (FAILED(dx.GetDX11Device()->CreateTexture2D(&desc, nullptr, &staging))) return false;
        staging_w = src.Width;
        staging_h = src.Height;
        staging_format = src.Format;
        return true;
    }
};

extern "C" {

BoSender* bo_sender_new(void* d3d11_device, const char* name) {
    BoSender* sender = new (std::nothrow) BoSender();
    if (!sender) return nullptr;
    if (name && name[0]) sender->dx.SetSenderName(name);
    // Spout's frame counter needs a per-user registry value (written by its SpoutSettings app), so it is off on a clean
    // machine and `IsFrameNew` is then always true. The sync event is an in-process opt-in that needs no registry.
    sender->dx.frame.EnableFrameSync(true);
    sender->dx.SetSenderFormat(DXGI_FORMAT_B8G8R8A8_UNORM);
    if (!sender->dx.OpenDirectX11(static_cast<ID3D11Device*>(d3d11_device))) {
        delete sender;
        return nullptr;
    }
    return sender;
}

void bo_sender_free(BoSender* sender) {
    if (!sender) return;
    sender->dx.ReleaseSender();
    delete sender;
}

int bo_sender_send_texture(BoSender* sender, void* texture) {
    return sender && sender->dx.SendTexture(static_cast<ID3D11Texture2D*>(texture)) ? 1 : 0;
}

int bo_sender_send_image(BoSender* sender, const uint8_t* pixels, uint32_t width, uint32_t height, uint32_t pitch) {
    return sender && sender->dx.SendImage(pixels, width, height, pitch) ? 1 : 0;
}

int bo_sender_wait_gpu(BoSender* sender) {
    if (!sender) return 0;
    ID3D11Device* device = sender->dx.GetDX11Device();
    ID3D11DeviceContext* context = sender->dx.GetDX11Context();
    if (!device || !context) return 0;
    if (!sender->gpu_done) {
        D3D11_QUERY_DESC desc = {D3D11_QUERY_EVENT, 0};
        if (FAILED(device->CreateQuery(&desc, &sender->gpu_done))) return 0;
    }
    context->End(sender->gpu_done);
    context->Flush();
    BOOL done = FALSE;
    const DWORD start = GetTickCount();
    while (context->GetData(sender->gpu_done, &done, sizeof(done), 0) == S_FALSE) {
        if (GetTickCount() - start > 1000) return 0;
        SwitchToThread();
    }
    return done ? 1 : 0;
}

void bo_sender_signal_sync(BoSender* sender) {
    if (sender) sender->dx.SetFrameSync(sender->dx.GetName());
}

const char* bo_sender_name(const BoSender* sender) {
    if (!sender) return nullptr;
    // GetName is not const in the SDK, but it only reads.
    const char* name = const_cast<spoutDX&>(sender->dx).GetName();
    return (name && name[0]) ? name : nullptr;
}

BoReceiver* bo_receiver_new(const char* name) {
    BoReceiver* receiver = new (std::nothrow) BoReceiver();
    if (!receiver) return nullptr;
    receiver->dx.frame.EnableFrameSync(true);
    if (name && name[0]) {
        receiver->wanted = name;
        receiver->dx.SetReceiverName(name);
    }
    return receiver;
}

void bo_receiver_free(BoReceiver* receiver) {
    if (!receiver) return;
    receiver->dx.ReleaseReceiver();
    delete receiver;
}

int bo_receiver_wait_sync(BoReceiver* receiver, uint32_t timeout_ms) {
    if (!receiver || receiver->wanted.empty()) return 0;
    return receiver->dx.WaitFrameSync(receiver->wanted.c_str(), timeout_ms) ? 1 : 0;
}

int bo_receiver_poll(BoReceiver* receiver, BoReceiveInfo* info) {
    if (!receiver || !info) return 0;
    *info = BoReceiveInfo{};
    const bool connected = receiver->dx.ReceiveTexture();
    info->connected = connected ? 1 : 0;
    if (!connected) return 0;
    if (receiver->dx.IsUpdated()) {
        // A new or resized sender: the SDK re-creates its texture on the next call, so the caller polls again.
        info->updated = 1;
    } else {
        info->new_frame = receiver->dx.IsFrameNew() ? 1 : 0;
    }
    info->width = receiver->dx.GetSenderWidth();
    info->height = receiver->dx.GetSenderHeight();
    info->dxgi_format = static_cast<uint32_t>(receiver->dx.GetSenderFormat());
    info->sender_frame = static_cast<uint64_t>(receiver->dx.GetSenderFrame());
    info->sender_fps = receiver->dx.GetSenderFps();
    return 1;
}

int bo_receiver_read_pixels(BoReceiver* receiver, uint8_t* dst, size_t dst_len) {
    if (!receiver || !dst) return 0;
    ID3D11Texture2D* texture = receiver->dx.GetSenderTexture();
    ID3D11DeviceContext* context = receiver->dx.GetDX11Context();
    if (!texture || !context) return 0;

    D3D11_TEXTURE2D_DESC desc = {};
    texture->GetDesc(&desc);
    const size_t row_bytes = static_cast<size_t>(desc.Width) * 4;  // 8-bit four-channel formats only
    if (dst_len < row_bytes * desc.Height || !receiver->ensure_staging(desc)) return 0;

    context->CopyResource(receiver->staging, texture);
    D3D11_MAPPED_SUBRESOURCE mapped = {};
    // Map blocks until the GPU has finished the copy, so a successful read means the pixels are real.
    if (FAILED(context->Map(receiver->staging, 0, D3D11_MAP_READ, 0, &mapped))) return 0;
    const uint8_t* src = static_cast<const uint8_t*>(mapped.pData);
    for (UINT y = 0; y < desc.Height; ++y) {
        memcpy(dst + y * row_bytes, src + static_cast<size_t>(y) * mapped.RowPitch, row_bytes);
    }
    context->Unmap(receiver->staging, 0);
    return 1;
}

}  // extern "C"
