//! GPU time per frame from timestamp queries, and the process's CPU use, for the 1080p / 30 fps cost measurement.

use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};

use crate::stats::Series;

/// Frames that can be in flight at once, each with its own query set and readback buffer.
const SLOTS: usize = 4;

/// Two timestamps per frame: the start of the first pass and the end of the last.
const QUERIES: u32 = 2;

/// Bytes the two resolved timestamps take.
const QUERY_BYTES: u64 = QUERIES as u64 * size_of::<u64>() as u64;

/// What the map callback of a slot has reported.
const WAITING: u8 = 0;
const MAPPED: u8 = 1;
const MAP_FAILED: u8 = 2;

struct Slot {
    queries: wgpu::QuerySet,
    resolve: wgpu::Buffer,
    read: wgpu::Buffer,
    /// Set by the map callback: [`MAPPED`] once `read` holds this frame's timestamps, [`MAP_FAILED`] if mapping failed.
    state: Arc<AtomicU8>,
    in_flight: bool,
}

/// Measures how long the GPU spends on a frame, without ever waiting for it: each frame writes into a free slot and the
/// result is picked up a few frames later.
pub struct GpuTimer {
    period_ns: f32,
    slots: Vec<Slot>,
    next: usize,
    samples_ms: Vec<f64>,
    failures: u32,
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
                state: Arc::new(AtomicU8::new(WAITING)),
                in_flight: false,
            })
            .collect();
        Self {
            period_ns: queue.get_timestamp_period(),
            slots,
            next: 0,
            // Room for every slot finishing in one call, so collecting never grows it.
            samples_ms: Vec::with_capacity(SLOTS),
            failures: 0,
        }
    }

    /// Collects finished frames and returns a free slot for the next one, or `None` when every slot is still waiting for
    /// the GPU (that frame is then simply not measured).
    pub fn begin(&mut self, device: &wgpu::Device) -> Option<usize> {
        // A failed poll only means nothing finished yet; the slots are checked by their own flags.
        let _ = device.poll(wgpu::PollType::Poll);
        for slot in &mut self.slots {
            if !slot.in_flight {
                continue;
            }
            match slot.state.load(Ordering::Acquire) {
                MAPPED => {
                    match slot.read.slice(..).get_mapped_range() {
                        Ok(data) => {
                            let ticks: &[u64] = bytemuck::cast_slice(&data);
                            // Wrapping: the counter may roll over between the two writes.
                            let elapsed = ticks[1].wrapping_sub(ticks[0]);
                            self.samples_ms.push(elapsed as f64 * f64::from(self.period_ns) / 1.0e6);
                        }
                        Err(_) => self.failures += 1,
                    }
                    slot.read.unmap();
                }
                // The buffer was never mapped, so there is nothing to unmap; the slot is free again.
                MAP_FAILED => self.failures += 1,
                _ => continue,
            }
            slot.state.store(WAITING, Ordering::Release);
            slot.in_flight = false;
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
        let state = s.state.clone();
        s.read.slice(..).map_async(wgpu::MapMode::Read, move |result| {
            state.store(if result.is_ok() { MAPPED } else { MAP_FAILED }, Ordering::Release);
        });
        s.in_flight = true;
    }

    /// Moves the GPU milliseconds measured since the last call into `series`, keeping this buffer's allocation.
    pub fn drain_into(&mut self, series: &mut Series) {
        series.extend_ms(self.samples_ms.drain(..));
    }

    /// Frames whose timestamps could not be read back.
    pub fn failures(&self) -> u32 {
        self.failures
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
