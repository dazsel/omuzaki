//! The Windows DX12 lane: the worker renders into textures other processes can open, the host imports them, keeps the
//! last frame in a texture of its own, and sends that texture to Spout.
//!
//! Written and compile-checked on Linux (`tools/check-windows.sh`); **never run**. Every claim about how it behaves on a
//! real GPU is for the owner run to confirm.
//!
//! Hand-off, per frame:
//! 1. the worker draws the scene, copies it into slot `n` (a shared texture), and signals a shared fence with `index + 1`;
//! 2. it posts `(slot, index)` over the pipe;
//! 3. the host makes its own D3D12 queue wait for the fence (GPU side, no CPU wait), then copies the slot into its
//!    *hold texture*, which the host owns and which survives the worker;
//! 4. every output tick the host sends the hold texture through `spout-out`'s `D3D11On12` bridge, fresh or not.
//!
//! The shared slots are plain D3D12 committed resources created with `ALLOW_SIMULTANEOUS_ACCESS`, shared by NT handle
//! under a name both sides can compute. A keyed mutex would need a D3D11-created texture; the fence does the same job
//! and keeps the renderer on D3D12.

pub mod host;
pub mod shared;
pub mod worker;

/// Slots in the worker's ring of shared textures.
pub const SLOTS: u32 = 3;

/// The object-namespace name of slot `slot` (or the fence when `slot` is `None`) for worker `worker` of host `host`.
/// The host's process id keeps two hosts on one machine apart.
#[must_use]
pub fn object_name(host: u32, worker: u32, slot: Option<u32>) -> String {
    match slot {
        Some(slot) => format!("Local\\omuzaki-wp-{host}-{worker}-slot{slot}"),
        None => format!("Local\\omuzaki-wp-{host}-{worker}-fence"),
    }
}

#[cfg(test)]
mod tests {
    use super::object_name;

    #[test]
    fn names_differ_per_worker_slot_and_fence() {
        let names = [
            object_name(1, 1, Some(0)),
            object_name(1, 1, Some(1)),
            object_name(1, 2, Some(0)),
            object_name(1, 1, None),
        ];
        for (i, a) in names.iter().enumerate() {
            for b in &names[i + 1..] {
                assert_ne!(a, b);
            }
        }
    }
}
