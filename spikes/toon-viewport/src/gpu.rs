//! Adapter and device setup shared by the window and the headless mode.

/// A device and queue, plus what they run on.
pub struct Gpu {
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub info: wgpu::AdapterInfo,
}

/// Why no GPU could be opened.
#[derive(Debug, thiserror::Error)]
pub enum GpuError {
    /// No adapter matched.
    #[error("no suitable GPU adapter: {0}")]
    Adapter(#[from] wgpu::RequestAdapterError),
    /// The adapter refused the device.
    #[error("opening the GPU device: {0}")]
    Device(#[from] wgpu::RequestDeviceError),
}

/// The device the viewport wants from `adapter`: its own limits, and timestamp queries when it has them.
#[must_use]
pub fn device_descriptor(adapter: &wgpu::Adapter) -> wgpu::DeviceDescriptor<'static> {
    wgpu::DeviceDescriptor {
        label: Some("toon viewport"),
        required_features: adapter.features() & wgpu::Features::TIMESTAMP_QUERY,
        required_limits: adapter.limits(),
        ..Default::default()
    }
}

/// Opens a GPU without a window. `WGPU_BACKEND` and `WGPU_POWER_PREF` select the adapter as usual.
///
/// # Errors
/// Fails when no adapter or device is available.
pub fn open_headless() -> Result<Gpu, GpuError> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env());
    let adapter = pollster::block_on(instance.request_adapter(&wgpu::RequestAdapterOptions {
        power_preference: wgpu::PowerPreference::from_env().unwrap_or(wgpu::PowerPreference::HighPerformance),
        ..Default::default()
    }))?;
    let (device, queue) = pollster::block_on(adapter.request_device(&device_descriptor(&adapter)))?;
    Ok(Gpu { device, queue, info: adapter.get_info() })
}
