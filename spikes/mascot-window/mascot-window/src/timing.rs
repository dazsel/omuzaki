//! GPU time per frame from timestamp queries, and the process's CPU use, for the 1080p / 30 fps cost measurement.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// Frames that can be in flight at once, each with its own query set and readback buffer.
const SLOTS: usize = 4;

/// Two timestamps per frame: the start of the first pass and the end of the last.
const QUERIES: u32 = 2;

/// Bytes the two resolved timestamps take.
const QUERY_BYTES: u64 = QUERIES as u64 * size_of::<u64>() as u64;

struct Slot {
    queries: wgpu::QuerySet,
    resolve: wgpu::Buffer,
    read: wgpu::Buffer,
    /// Set by the map callback once `read` holds this frame's timestamps.
    mapped: Arc<AtomicBool>,
    in_flight: bool,
}

/// Measures how long the GPU spends on a frame, without ever waiting for it: each frame writes into a free slot and the
/// result is picked up a few frames later.
pub struct GpuTimer {
    period_ns: f32,
    slots: Vec<Slot>,
    next: usize,
    samples_ms: Vec<f64>,
}

impl GpuTimer {
    /// Builds the slots. The device must have been created with [`wgpu::Features::TIMESTAMP_QUERY`].
    pub fn new(device: &wgpu::Device, queue: &wgpu::Queue) -> Self {
        let slots = (0..SLOTS)
            .map(|_| Slot {
                queries: device.create_query_set(&wgpu::QuerySetDescriptor {
                    label: Some("frame timestamps"),
                    ty: wgpu::QueryType::Timestamp,
                    count: QUERIES,
                }),
                resolve: device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("timestamp resolve"),
                    size: QUERY_BYTES,
                    usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
                    mapped_at_creation: false,
                }),
                read: device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("timestamp readback"),
                    size: QUERY_BYTES,
                    usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                    mapped_at_creation: false,
                }),
                mapped: Arc::new(AtomicBool::new(false)),
                in_flight: false,
            })
            .collect();
        Self { period_ns: queue.get_timestamp_period(), slots, next: 0, samples_ms: Vec::new() }
    }

    /// Collects finished frames and returns a free slot for the next one, or `None` when every slot is still waiting for
    /// the GPU (that frame is then simply not measured).
    pub fn begin(&mut self, device: &wgpu::Device) -> Option<usize> {
        // A failed poll only means nothing finished yet; the slots are checked by their own flags.
        let _ = device.poll(wgpu::PollType::Poll);
        for slot in &mut self.slots {
            if slot.in_flight && slot.mapped.load(Ordering::Acquire) {
                if let Ok(data) = slot.read.slice(..).get_mapped_range() {
                    let ticks: &[u64] = bytemuck::cast_slice(&data);
                    // Wrapping: the counter may roll over between the two writes.
                    let elapsed = ticks[1].wrapping_sub(ticks[0]);
                    self.samples_ms.push(elapsed as f64 * f64::from(self.period_ns) / 1.0e6);
                }
                slot.read.unmap();
                slot.mapped.store(false, Ordering::Release);
                slot.in_flight = false;
            }
        }
        let free = (0..SLOTS).map(|i| (self.next + i) % SLOTS).find(|i| !self.slots[*i].in_flight)?;
        self.next = (free + 1) % SLOTS;
        Some(free)
    }

    /// The pass timestamp writes for `slot`: the first pass writes index 0 when it starts, the last writes index 1 when it
    /// ends.
    pub fn writes(&self, slot: usize, last_pass: bool) -> wgpu::RenderPassTimestampWrites<'_> {
        wgpu::RenderPassTimestampWrites {
            query_set: &self.slots[slot].queries,
            beginning_of_pass_write_index: (!last_pass).then_some(0),
            end_of_pass_write_index: last_pass.then_some(1),
        }
    }

    /// Records the copy of the resolved timestamps into the readback buffer. Call before submitting.
    pub fn resolve(&self, encoder: &mut wgpu::CommandEncoder, slot: usize) {
        let s = &self.slots[slot];
        encoder.resolve_query_set(&s.queries, 0..QUERIES, &s.resolve, 0);
        encoder.copy_buffer_to_buffer(&s.resolve, 0, &s.read, 0, QUERY_BYTES);
    }

    /// Starts mapping the readback buffer. Call after submitting.
    pub fn map(&mut self, slot: usize) {
        let s = &mut self.slots[slot];
        let flag = s.mapped.clone();
        s.read.slice(..).map_async(wgpu::MapMode::Read, move |result| {
            if result.is_ok() {
                flag.store(true, Ordering::Release);
            }
        });
        s.in_flight = true;
    }

    /// Takes the GPU milliseconds measured since the last call.
    pub fn take(&mut self) -> Vec<f64> {
        std::mem::take(&mut self.samples_ms)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn query_buffers_hold_two_u64() {
        assert_eq!(QUERY_BYTES, 16);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn cpu_seconds_grow_with_work() {
        use crate::platform::process_cpu_seconds;
        let before = process_cpu_seconds().unwrap();
        let mut x = 0_u64;
        let start = std::time::Instant::now();
        while start.elapsed().as_millis() < 120 {
            x = std::hint::black_box(x.wrapping_add(1));
        }
        assert!(process_cpu_seconds().unwrap() > before);
    }
}
