//! GPU frame timing with timestamp queries, read back without stalling the frame loop.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Measures the GPU time between the start of the first pass and the end of the last one in a frame.
pub(crate) struct GpuTimer {
    queries: wgpu::QuerySet,
    resolve_buffer: wgpu::Buffer,
    read: wgpu::Buffer,
    period_ns: f32,
    /// Set by the map callback once `read` holds a finished measurement.
    ready: Arc<AtomicBool>,
    /// A measurement is queued or mapped, so no new one may be started until it is collected.
    busy: bool,
}

impl GpuTimer {
    /// `None` when the device has no timestamp queries.
    pub(crate) fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Option<Self> {
        if !device.features().contains(wgpu::Features::TIMESTAMP_QUERY) {
            return None;
        }
        let size = 2 * std::mem::size_of::<u64>() as u64;
        let buffer = |label, usage| {
            device.create_buffer(&wgpu::BufferDescriptor { label: Some(label), size, usage, mapped_at_creation: false })
        };
        Some(Self {
            queries: device.create_query_set(&wgpu::QuerySetDescriptor {
                label: Some("frame timestamps"),
                ty: wgpu::QueryType::Timestamp,
                count: 2,
            }),
            resolve_buffer: buffer(
                "timestamp resolve",
                wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
            ),
            read: buffer("timestamp read", wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ),
            period_ns: queue.get_timestamp_period(),
            ready: Arc::new(AtomicBool::new(false)),
            busy: false,
        })
    }

    /// Timestamp writes for the first pass of the frame.
    pub(crate) fn first_pass(&self) -> wgpu::RenderPassTimestampWrites<'_> {
        wgpu::RenderPassTimestampWrites {
            query_set: &self.queries,
            beginning_of_pass_write_index: Some(0),
            end_of_pass_write_index: None,
        }
    }

    /// Timestamp writes for the last pass of the frame.
    pub(crate) fn last_pass(&self) -> wgpu::RenderPassTimestampWrites<'_> {
        wgpu::RenderPassTimestampWrites {
            query_set: &self.queries,
            beginning_of_pass_write_index: None,
            end_of_pass_write_index: Some(1),
        }
    }

    /// Queues the readback of this frame's timestamps. Returns whether it did (it does not while an earlier
    /// measurement is outstanding); when it did, call [`GpuTimer::request_map`] after the submit.
    pub(crate) fn resolve(&mut self, encoder: &mut wgpu::CommandEncoder) -> bool {
        if self.busy {
            return false;
        }
        encoder.resolve_query_set(&self.queries, 0..2, &self.resolve_buffer, 0);
        encoder.copy_buffer_to_buffer(&self.resolve_buffer, 0, &self.read, 0, self.read.size());
        self.busy = true;
        true
    }

    /// Asks for the readback buffer to be mapped; call once per successful [`GpuTimer::resolve`], after submit.
    pub(crate) fn request_map(&self) {
        let ready = Arc::clone(&self.ready);
        self.read
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| ready.store(result.is_ok(), Ordering::Release));
    }

    /// The GPU time of the measured frame in milliseconds, once its mapping has completed.
    pub(crate) fn collect(&mut self) -> Option<f32> {
        if !self.ready.swap(false, Ordering::AcqRel) {
            return None;
        }
        let ms = {
            let data = self.read.slice(..).get_mapped_range().ok()?;
            let ticks: &[u64] = bytemuck::cast_slice(&data);
            // Timestamps are counters of a few seconds at most, so the f32 conversion loses nothing visible.
            #[allow(clippy::cast_precision_loss)]
            let delta = ticks[1].saturating_sub(ticks[0]) as f32;
            delta * self.period_ns / 1.0e6
        };
        self.read.unmap();
        self.busy = false;
        Some(ms)
    }
}
