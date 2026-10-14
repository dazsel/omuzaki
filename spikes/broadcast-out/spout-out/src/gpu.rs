//! wgpu adapter and device setup.

/// A device and queue, plus what they run on.
pub struct Gpu {
    /// The logical device.
    pub device: wgpu::Device,
    /// Its queue.
    pub queue: wgpu::Queue,
    /// Adapter name, for reports.
    pub adapter: String,
    /// The backend that was picked.
    pub backend: wgpu::Backend,
}

/// Why no GPU could be opened.
#[derive(Debug, thiserror::Error)]
pub enum GpuError {
    /// No adapter matched the requested backends.
    #[error("no suitable GPU adapter: {0}")]
    Adapter(#[from] wgpu::RequestAdapterError),
    /// The adapter refused the device.
    #[error("opening the GPU device: {0}")]
    Device(#[from] wgpu::RequestDeviceError),
}

/// Opens the high-performance adapter among `backends`, without a window.
///
/// # Errors
/// Fails when no adapter or device is available.
pub fn open(backends: wgpu::Backends) -> Result<Gpu, GpuError> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends,
        ..wgpu::InstanceDescriptor::new_without_display_handle()
    });
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::HighPerformance,
        ..Default::default()
    }))?;
    let info = adapter.get_info();
    let (device, queue) = pollster::block_on(adapter.request_device(&wgpu::DeviceDescriptor {
        label: Some("broadcast-out"),
        required_limits: wgpu::Limits::default(),
        ..Default::default()
    }))?;
    Ok(Gpu { device, queue, adapter: info.name, backend: info.backend })
}
