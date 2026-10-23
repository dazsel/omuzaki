//! The macOS half of the Syphon server: loading the framework, the Metal texture, and `SyphonMetalServer`.

// Syphon is an Objective-C framework loaded with `dlopen` and messaged by selector; Metal's texture writes take raw
// pointers. Every unsafe block says what it relies on.
#![allow(unsafe_code)]

use std::ffi::{CString, c_void};
use std::os::unix::ffi::OsStrExt as _;
use std::ptr::NonNull;
use std::sync::OnceLock;

use objc2::encode::{Encode, Encoding};
use objc2::msg_send;
use objc2::rc::{Allocated, Retained, autoreleasepool};
use objc2::runtime::{AnyClass, AnyObject, Bool, ProtocolObject};
use objc2_foundation::NSString;
use objc2_metal::{
    MTLCommandBuffer, MTLCommandQueue, MTLCreateSystemDefaultDevice, MTLDevice, MTLOrigin, MTLPixelFormat, MTLRegion,
    MTLSize, MTLStorageMode, MTLTexture, MTLTextureDescriptor, MTLTextureUsage,
};
use omuzaki_core::glam::UVec2;
use omuzaki_stream_api::error::StreamError;

use crate::share::Publisher;

/// Whether the framework loaded, decided once: a loaded framework's classes stay registered for the process's life.
static LOADED: OnceLock<Result<(), StreamError>> = OnceLock::new();

/// Loads `Syphon.framework` from the first place [`super::framework_candidates`] names that has it.
pub(super) fn load() -> Result<(), StreamError> {
    LOADED
        .get_or_init(|| {
            let env = std::env::var_os(super::FRAMEWORK_ENV).map(Into::into);
            let exe = std::env::current_exe().ok();
            for framework in super::framework_candidates(env, exe.as_deref()) {
                let Ok(binary) = CString::new(framework.join("Syphon").as_os_str().as_bytes()) else { continue };
                // SAFETY: a NUL-terminated path; the handle is kept for the process's life (never dlclose'd), since
                // the framework registers Objective-C classes the runtime cannot unregister.
                let handle = unsafe { libc::dlopen(binary.as_ptr(), libc::RTLD_NOW | libc::RTLD_GLOBAL) };
                if !handle.is_null() && AnyClass::get(c"SyphonMetalServer").is_some() {
                    return Ok(());
                }
            }
            Err(StreamError::Unavailable {
                what: "Syphon".into(),
                hint: format!(
                    "Syphon.framework was not found: it ships inside Omuzaki.app; for a build run from source, put it \
                     in /Library/Frameworks or set {} to its folder",
                    super::FRAMEWORK_ENV
                ),
            })
        })
        .clone()
}

/// `NSRect`, which is `CGRect` on 64-bit macOS: an origin and a size, four doubles.
#[repr(C)]
#[derive(Clone, Copy)]
struct Rect {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
}

// SAFETY: laid out exactly as `CGRect { CGPoint origin; CGSize size; }` of `CGFloat`s, which are doubles on 64-bit.
unsafe impl Encode for Rect {
    const ENCODING: Encoding = Encoding::Struct(
        "CGRect",
        &[
            Encoding::Struct("CGPoint", &[Encoding::Double, Encoding::Double]),
            Encoding::Struct("CGSize", &[Encoding::Double, Encoding::Double]),
        ],
    );
}

fn io(what: &str) -> StreamError {
    StreamError::Io(format!("Syphon: {what}"))
}

/// A running Syphon server. Dropping it stops the server, which takes it off every client's list.
pub(super) struct Server {
    syphon: Retained<AnyObject>,
    texture: Retained<ProtocolObject<dyn MTLTexture>>,
    queue: Retained<ProtocolObject<dyn MTLCommandQueue>>,
    size: UVec2,
}

impl Server {
    pub(super) fn open(name: &str, size: UVec2) -> Result<Self, StreamError> {
        load()?;
        let device = MTLCreateSystemDefaultDevice()
            .ok_or_else(|| StreamError::Unavailable { what: "Metal".into(), hint: "this Mac reports no GPU".into() })?;
        let queue = device.newCommandQueue().ok_or_else(|| io("no command queue"))?;
        // SAFETY: a 2D BGRA8 descriptor of a size the config validated (non-zero), with no mipmaps.
        let descriptor = unsafe {
            MTLTextureDescriptor::texture2DDescriptorWithPixelFormat_width_height_mipmapped(
                MTLPixelFormat::BGRA8Unorm,
                size.x as usize,
                size.y as usize,
                false,
            )
        };
        // Managed: the CPU writes it with `replaceRegion`, and Metal moves the bytes to the GPU before the copy reads.
        descriptor.setStorageMode(MTLStorageMode::Managed);
        descriptor.setUsage(MTLTextureUsage::ShaderRead);
        let texture = device.newTextureWithDescriptor(&descriptor).ok_or_else(|| io("no texture"))?;
        let class = AnyClass::get(c"SyphonMetalServer").ok_or_else(|| io("the framework has no SyphonMetalServer"))?;
        let name = NSString::from_str(name);
        // SAFETY: `+alloc` then `-initWithName:device:options:`, SyphonMetalServer's designated initialiser, with an
        // NSString, a live MTLDevice and nil options; it returns nil on failure.
        let server: Option<Retained<AnyObject>> = unsafe {
            let allocated: Allocated<AnyObject> = msg_send![class, alloc];
            msg_send![allocated, initWithName: &*name, device: &*device, options: None::<&AnyObject>]
        };
        let syphon = server.ok_or_else(|| io("the server did not start"))?;
        Ok(Self { syphon, texture, queue, size })
    }
}

impl Publisher for Server {
    fn publish(&mut self, bgra: &[u8]) -> Result<(), StreamError> {
        autoreleasepool(|_| {
            let (width, height) = (self.size.x as usize, self.size.y as usize);
            let region =
                MTLRegion { origin: MTLOrigin { x: 0, y: 0, z: 0 }, size: MTLSize { width, height, depth: 1 } };
            let bytes = NonNull::new(bgra.as_ptr().cast_mut().cast::<c_void>()).ok_or_else(|| io("no pixels"))?;
            // SAFETY: `bgra` holds `height` rows of `width * 4` bytes (the share module sizes it from the config), the
            // texture is that size, and the previous frame's copy finished before this call (see below).
            unsafe { self.texture.replaceRegion_mipmapLevel_withBytes_bytesPerRow(region, 0, bytes, width * 4) };
            let commands = self.queue.commandBuffer().ok_or_else(|| io("no command buffer"))?;
            let rect = Rect { x: 0.0, y: 0.0, width: f64::from(self.size.x), height: f64::from(self.size.y) };
            // SAFETY: SyphonMetalServer's `-publishFrameTexture:onCommandBuffer:imageRegion:flipped:` with a live
            // texture, a fresh command buffer, the whole texture as the region, and Metal's own top-down rows.
            unsafe {
                let () = msg_send![
                    &*self.syphon,
                    publishFrameTexture: &*self.texture,
                    onCommandBuffer: &*commands,
                    imageRegion: rect,
                    flipped: Bool::NO
                ];
            }
            commands.commit();
            // The server's copy reads the texture on the GPU; the next write must not land under it.
            commands.waitUntilCompleted();
            Ok(())
        })
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        // SAFETY: `-stop` takes the server off the clients' lists and releases its surfaces; it takes no arguments.
        unsafe {
            let () = msg_send![&*self.syphon, stop];
        }
    }
}
