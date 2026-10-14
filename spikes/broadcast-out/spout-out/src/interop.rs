//! The D3D12 → D3D11 bridge that lets Spout (a Direct3D 11 library) send a texture wgpu rendered on Direct3D 12.
//!
//! wgpu's DX12 backend owns an `ID3D12Device` and an `ID3D12CommandQueue`. Spout wants an `ID3D11Texture2D`. Microsoft's
//! `D3D11On12` layer gives a D3D11 device that records onto the same D3D12 queue, and lets us wrap a D3D12 resource as a
//! D3D11 texture. Because both devices submit to one queue, the order of submission is the order of execution: the
//! copy Spout records runs after wgpu's render and before wgpu's next frame, with no CPU wait. This is the route the Spout
//! SDK itself documents for D3D12 (`SpoutDX12`).
//!
//! The wrapped texture is the scene's render target. wgpu leaves it in `RENDER_TARGET` state after the render pass, so the
//! wrapper is declared with that as both the state to acquire it in and the state to give it back in. wgpu never sees the
//! state change.

#![allow(unsafe_code)]

use std::cell::Cell;

use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
use windows::Win32::Graphics::Direct3D11::{
    D3D11_CREATE_DEVICE_BGRA_SUPPORT, ID3D11Device, ID3D11DeviceContext, ID3D11Resource, ID3D11Texture2D,
};
use windows::Win32::Graphics::Direct3D11on12::{D3D11_RESOURCE_FLAGS, D3D11On12CreateDevice, ID3D11On12Device};
use windows::Win32::Graphics::Direct3D12::{
    D3D12_FENCE_FLAG_NONE, D3D12_RESOURCE_STATE_RENDER_TARGET, ID3D12CommandQueue, ID3D12Device, ID3D12Fence,
    ID3D12Resource,
};
use windows::Win32::System::Threading::{CreateEventW, WaitForSingleObject};
use windows::core::{IUnknown, Interface};

use crate::spout::{SpoutError, SpoutSender};

/// Why the bridge could not be built or used.
#[derive(Debug, thiserror::Error)]
pub enum BridgeError {
    /// wgpu is not running on Direct3D 12.
    #[error("the wgpu device is not a Direct3D 12 device")]
    NotDx12,
    /// A Direct3D or Win32 call failed.
    #[error("{what}: {source}")]
    Win32 {
        /// The call that failed.
        what: &'static str,
        /// The HRESULT error.
        source: windows::core::Error,
    },
    /// Waiting for the GPU fence timed out or failed.
    #[error("waiting for the GPU timed out")]
    FenceTimeout,
}

fn win32(what: &'static str) -> impl FnOnce(windows::core::Error) -> BridgeError {
    move |source| BridgeError::Win32 { what, source }
}

/// A D3D11 view of wgpu's D3D12 render target, plus a fence on wgpu's queue.
pub struct SpoutBridge {
    device11: ID3D11Device,
    context11: ID3D11DeviceContext,
    on12: ID3D11On12Device,
    wrapped_texture: ID3D11Texture2D,
    wrapped_resource: ID3D11Resource,
    queue12: ID3D12CommandQueue,
    fence: ID3D12Fence,
    fence_event: HANDLE,
    fence_value: Cell<u64>,
}

impl SpoutBridge {
    /// Builds the bridge for `texture`, which must belong to `device`/`queue` running on the DX12 backend and have been
    /// created with `RENDER_ATTACHMENT` (so wgpu leaves it in the render-target state).
    ///
    /// # Errors
    /// Fails when wgpu is not on DX12 or any Direct3D call is refused.
    pub fn new(device: &wgpu::Device, texture: &wgpu::Texture) -> Result<Self, BridgeError> {
        // SAFETY: the hal handles are only borrowed to clone the COM pointers (AddRef); they are not used after.
        let (device12, queue12, resource12) = unsafe {
            let hal_device = device.as_hal::<wgpu::hal::api::Dx12>().ok_or(BridgeError::NotDx12)?;
            let hal_texture = texture.as_hal::<wgpu::hal::api::Dx12>().ok_or(BridgeError::NotDx12)?;
            let device12: ID3D12Device = hal_device.raw_device().clone();
            let queue12: ID3D12CommandQueue = hal_device.raw_queue().clone();
            let resource12: ID3D12Resource = hal_texture.raw_resource().clone();
            (device12, queue12, resource12)
        };

        let queue_unknown: IUnknown = queue12.cast().map_err(win32("queue as IUnknown"))?;
        let mut device11: Option<ID3D11Device> = None;
        let mut context11: Option<ID3D11DeviceContext> = None;
        // SAFETY: the out-pointers reference live locals; `device12` and the queue are valid COM objects.
        unsafe {
            D3D11On12CreateDevice(
                &device12,
                D3D11_CREATE_DEVICE_BGRA_SUPPORT.0,
                None,
                Some(&[Some(queue_unknown)]),
                0,
                Some(&raw mut device11),
                Some(&raw mut context11),
                None,
            )
        }
        .map_err(win32("D3D11On12CreateDevice"))?;
        let (Some(device11), Some(context11)) = (device11, context11) else {
            return Err(BridgeError::NotDx12);
        };
        let on12: ID3D11On12Device = device11.cast().map_err(win32("ID3D11On12Device"))?;

        let flags = D3D11_RESOURCE_FLAGS { BindFlags: 0, MiscFlags: 0, CPUAccessFlags: 0, StructureByteStride: 0 };
        let mut wrapped: Option<ID3D11Texture2D> = None;
        // SAFETY: `flags` and `wrapped` are live locals; `resource12` is the render target's D3D12 resource.
        unsafe {
            on12.CreateWrappedResource(
                &resource12,
                &raw const flags,
                D3D12_RESOURCE_STATE_RENDER_TARGET,
                D3D12_RESOURCE_STATE_RENDER_TARGET,
                &raw mut wrapped,
            )
        }
        .map_err(win32("CreateWrappedResource"))?;
        let wrapped_texture = wrapped.ok_or(BridgeError::NotDx12)?;
        let wrapped_resource: ID3D11Resource = wrapped_texture.cast().map_err(win32("ID3D11Resource"))?;

        // SAFETY: plain object creation with valid arguments.
        let fence: ID3D12Fence =
            unsafe { device12.CreateFence(0, D3D12_FENCE_FLAG_NONE) }.map_err(win32("CreateFence"))?;
        // SAFETY: default security, auto-reset, initially non-signalled, unnamed.
        let fence_event = unsafe { CreateEventW(None, false, false, None) }.map_err(win32("CreateEventW"))?;

        Ok(Self {
            device11,
            context11,
            on12,
            wrapped_texture,
            wrapped_resource,
            queue12,
            fence,
            fence_event,
            fence_value: Cell::new(0),
        })
    }

    /// Creates a Spout sender whose shared texture lives on this bridge's `D3D11On12` device. The sender borrows the
    /// bridge, so the device cannot be dropped out from under it.
    ///
    /// # Errors
    /// Fails on a bad name or when Spout cannot create the sender.
    pub fn create_sender(&self, name: &str) -> Result<BridgedSender<'_>, SpoutError> {
        // SAFETY: `device11` is a live ID3D11Device owned by `self`, and the returned sender borrows `self`.
        let sender = unsafe { SpoutSender::on_device(self.device11.as_raw(), name)? };
        Ok(BridgedSender { sender, bridge: self })
    }

    /// Takes the texture for D3D11 use. wgpu must not touch it until [`Self::release`].
    fn acquire(&self) {
        // SAFETY: the resource was created by this `on12` device.
        unsafe { self.on12.AcquireWrappedResources(&[Some(self.wrapped_resource.clone())]) };
    }

    /// Gives the texture back and submits the D3D11 work (Spout's copy) to the shared D3D12 queue.
    fn release(&self) {
        // SAFETY: the resource was acquired by `acquire` on this `on12` device and `context11` belongs to it.
        unsafe {
            self.on12.ReleaseWrappedResources(&[Some(self.wrapped_resource.clone())]);
            self.context11.Flush();
        }
    }

    /// Blocks until everything submitted to wgpu's queue so far, including Spout's copy, has finished on the GPU.
    ///
    /// # Errors
    /// Fails when the fence cannot be signalled or the wait times out (one second).
    pub fn wait_gpu(&self) -> Result<(), BridgeError> {
        let value = self.fence_value.get() + 1;
        self.fence_value.set(value);
        // SAFETY: the fence, queue and event are owned by `self` and valid.
        unsafe {
            self.queue12.Signal(&self.fence, value).map_err(win32("queue Signal"))?;
            self.fence.SetEventOnCompletion(value, self.fence_event).map_err(win32("SetEventOnCompletion"))?;
            if WaitForSingleObject(self.fence_event, 1000) != WAIT_OBJECT_0 {
                return Err(BridgeError::FenceTimeout);
            }
        }
        Ok(())
    }
}

impl Drop for SpoutBridge {
    fn drop(&mut self) {
        // SAFETY: the event handle was created in `new` and is closed once. A failed close at teardown has no remedy.
        unsafe {
            let _ = CloseHandle(self.fence_event);
        }
    }
}

/// A Spout sender that sends the bridge's wrapped render target.
pub struct BridgedSender<'a> {
    sender: SpoutSender,
    bridge: &'a SpoutBridge,
}

impl BridgedSender<'_> {
    /// Sends the render target as it stands: takes it from wgpu, lets Spout copy it into its shared texture, gives it
    /// back and submits the copy to the shared queue. Call after the frame's `queue.submit`.
    ///
    /// # Errors
    /// Fails when Spout cannot create or fill its shared texture. The texture is handed back to wgpu either way.
    pub fn send(&mut self) -> Result<(), SpoutError> {
        self.bridge.acquire();
        // SAFETY: the pointer is the wrapped texture of the device the sender was created on, and it is acquired.
        let sent = unsafe { self.sender.send_texture(self.bridge.wrapped_texture.as_raw()) };
        self.bridge.release();
        sent
    }

    /// The underlying Spout sender (for the sync event and the registered name).
    pub fn sender(&mut self) -> &mut SpoutSender {
        &mut self.sender
    }
}
