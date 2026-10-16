//! Creating shared textures and fences (worker side) and opening them (host side), over D3D12 NT shared handles.

#![allow(unsafe_code)]

use windows::Win32::Foundation::{CloseHandle, GENERIC_ALL, HANDLE};
use windows::Win32::Graphics::Direct3D12::{
    D3D12_FENCE_FLAG_SHARED, D3D12_HEAP_FLAG_SHARED, D3D12_HEAP_PROPERTIES, D3D12_HEAP_TYPE_DEFAULT,
    D3D12_RESOURCE_DESC, D3D12_RESOURCE_DIMENSION_TEXTURE2D, D3D12_RESOURCE_FLAG_ALLOW_RENDER_TARGET,
    D3D12_RESOURCE_FLAG_ALLOW_SIMULTANEOUS_ACCESS, D3D12_RESOURCE_STATE_COMMON, D3D12_TEXTURE_LAYOUT_UNKNOWN,
    ID3D12Device, ID3D12Fence, ID3D12Resource,
};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::core::{HSTRING, PCWSTR};

use crate::Error;

fn win32(what: &'static str) -> impl FnOnce(windows::core::Error) -> Error {
    move |source| Error::Win32 { what, source }
}

/// The D3D12 device and queue under a wgpu DX12 device, as owned COM pointers.
///
/// # Errors
/// Fails when wgpu is not running on Direct3D 12.
pub fn raw_device_and_queue(
    device: &wgpu::Device,
) -> Result<(ID3D12Device, windows::Win32::Graphics::Direct3D12::ID3D12CommandQueue), Error> {
    // SAFETY: the hal handle is only borrowed to clone the COM pointers (AddRef); it is not used after.
    let hal = unsafe { device.as_hal::<wgpu::hal::api::Dx12>() }
        .ok_or_else(|| Error::Config("the wgpu device is not a Direct3D 12 device".into()))?;
    Ok((hal.raw_device().clone(), hal.raw_queue().clone()))
}

/// Creates a BGRA8 texture another process can open, and publishes it under `name`. The returned handle must stay open
/// as long as the name should resolve; it closes with the process.
///
/// # Errors
/// Fails when the device refuses the resource or the shared handle.
pub fn create_shared_texture(
    device: &ID3D12Device,
    size: (u32, u32),
    name: &str,
) -> Result<(ID3D12Resource, HANDLE), Error> {
    let heap = D3D12_HEAP_PROPERTIES { Type: D3D12_HEAP_TYPE_DEFAULT, ..Default::default() };
    let desc = D3D12_RESOURCE_DESC {
        Dimension: D3D12_RESOURCE_DIMENSION_TEXTURE2D,
        Alignment: 0,
        Width: u64::from(size.0),
        Height: size.1,
        DepthOrArraySize: 1,
        MipLevels: 1,
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Layout: D3D12_TEXTURE_LAYOUT_UNKNOWN,
        Flags: D3D12_RESOURCE_FLAG_ALLOW_RENDER_TARGET | D3D12_RESOURCE_FLAG_ALLOW_SIMULTANEOUS_ACCESS,
    };
    let mut resource: Option<ID3D12Resource> = None;
    // SAFETY: the descriptors and the out-pointer are live locals; the device is a valid COM object.
    unsafe {
        device.CreateCommittedResource(
            &raw const heap,
            D3D12_HEAP_FLAG_SHARED,
            &raw const desc,
            D3D12_RESOURCE_STATE_COMMON,
            None,
            &raw mut resource,
        )
    }
    .map_err(win32("CreateCommittedResource (shared texture)"))?;
    let resource = resource.ok_or_else(|| Error::Config("CreateCommittedResource returned no resource".into()))?;
    let wide = HSTRING::from(name);
    // SAFETY: the resource is valid, the name is a NUL-terminated wide string that outlives the call.
    let handle = unsafe { device.CreateSharedHandle(&resource, None, GENERIC_ALL.0, PCWSTR(wide.as_ptr())) }
        .map_err(win32("CreateSharedHandle (texture)"))?;
    Ok((resource, handle))
}

/// Creates a fence another process can open and wait on, published under `name`.
///
/// # Errors
/// Fails when the device refuses the fence or the shared handle.
pub fn create_shared_fence(device: &ID3D12Device, name: &str) -> Result<(ID3D12Fence, HANDLE), Error> {
    // SAFETY: plain object creation with valid arguments.
    let fence: ID3D12Fence =
        unsafe { device.CreateFence(0, D3D12_FENCE_FLAG_SHARED) }.map_err(win32("CreateFence (shared)"))?;
    let wide = HSTRING::from(name);
    // SAFETY: the fence is valid, the name is a NUL-terminated wide string that outlives the call.
    let handle = unsafe { device.CreateSharedHandle(&fence, None, GENERIC_ALL.0, PCWSTR(wide.as_ptr())) }
        .map_err(win32("CreateSharedHandle (fence)"))?;
    Ok((fence, handle))
}

/// Opens a shared D3D12 object published under `name` by another process.
///
/// # Errors
/// Fails when no such name exists (the worker died or has not created it yet) or the device refuses it.
pub fn open_by_name<T: windows::core::Interface>(device: &ID3D12Device, name: &str) -> Result<T, Error> {
    let wide = HSTRING::from(name);
    // SAFETY: the name is a NUL-terminated wide string that outlives the call.
    let handle = unsafe { device.OpenSharedHandleByName(PCWSTR(wide.as_ptr()), GENERIC_ALL.0) }
        .map_err(win32("OpenSharedHandleByName"))?;
    let mut object: Option<T> = None;
    // SAFETY: `handle` was just returned by the device; the out-pointer is a live local.
    let opened = unsafe { device.OpenSharedHandle(handle, &raw mut object) };
    // SAFETY: the handle is ours and closed once; the opened object keeps its own reference to the allocation.
    let _ = unsafe { CloseHandle(handle) };
    opened.map_err(win32("OpenSharedHandle"))?;
    object.ok_or_else(|| Error::Config(format!("{name} opened to nothing")))
}
