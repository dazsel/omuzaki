//! The virtual camera's media source: the COM object Windows' Frame Server creates (through [`DllGetClassObject`]) and
//! reads frames from.
//!
//! Frame Server's contract (Microsoft's "Frame Server custom media source" page): an in-process COM server whose source
//! implements `IMFMediaSourceEx`, `IMFGetService` and `IKsControl`, whose streams implement `IMFMediaStream2`, with no seek
//! or pause, event queues from `MFCreateEventQueue`, and `MENewStream` posted when `Start` completes. Frame Server runs
//! the object as Local Service in session 0, so it must not write to the file system or registry.

#![allow(unsafe_code)]
// The `#[implement]` macro expands to `inline(always)`, reference-to-pointer casts and raw-pointer trait methods, and the
// COM trait signatures are fixed by windows-rs, so these lints fire on code we do not write.
#![allow(
    clippy::inline_always,
    clippy::borrow_as_ptr,
    clippy::ref_as_ptr,
    clippy::not_unsafe_ptr_arg_deref,
    clippy::needless_pass_by_value
)]

use std::ffi::c_void;
use std::sync::{Arc, Mutex, PoisonError};

use windows::Win32::Foundation::{
    CLASS_E_CLASSNOTAVAILABLE, CLASS_E_NOAGGREGATION, E_FAIL, E_INVALIDARG, E_POINTER, S_FALSE, S_OK,
};
use windows::Win32::Media::KernelStreaming::{IKsControl, IKsControl_Impl, KSIDENTIFIER, PINNAME_VIDEO_CAPTURE};
use windows::Win32::Media::MediaFoundation::{
    IMFAsyncCallback, IMFAsyncResult, IMFAttributes, IMFGetService, IMFGetService_Impl, IMFMediaEvent,
    IMFMediaEventGenerator_Impl, IMFMediaEventQueue, IMFMediaSource, IMFMediaSource_Impl, IMFMediaSourceEx,
    IMFMediaSourceEx_Impl, IMFMediaStream2, IMFMediaType, IMFPresentationDescriptor, IMFStreamDescriptor,
    MEDIA_EVENT_GENERATOR_GET_EVENT_FLAGS, MENewStream, MESourceStarted, MESourceStopped, MEUpdatedStream,
    MF_DEVICESTREAM_FRAMESERVER_SHARED, MF_DEVICESTREAM_STREAM_CATEGORY, MF_DEVICESTREAM_STREAM_ID,
    MF_DEVSOURCE_ATTRIBUTE_FRIENDLY_NAME, MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE,
    MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID, MF_E_INVALID_STATE_TRANSITION, MF_E_INVALIDREQUEST, MF_E_SHUTDOWN,
    MF_E_UNSUPPORTED_SERVICE, MF_E_UNSUPPORTED_TIME_FORMAT, MF_MT_ALL_SAMPLES_INDEPENDENT, MF_MT_AVG_BITRATE,
    MF_MT_DEFAULT_STRIDE, MF_MT_FRAME_RATE, MF_MT_FRAME_SIZE, MF_MT_INTERLACE_MODE, MF_MT_MAJOR_TYPE,
    MF_MT_PIXEL_ASPECT_RATIO, MF_MT_SAMPLE_SIZE, MF_MT_SUBTYPE, MF_MT_TRANSFER_FUNCTION, MF_MT_VIDEO_NOMINAL_RANGE,
    MF_MT_VIDEO_PRIMARIES, MF_MT_YUV_MATRIX, MFCreateAttributes, MFCreateEventQueue, MFCreateMediaType,
    MFCreatePresentationDescriptor, MFCreateStreamDescriptor, MFMEDIASOURCE_IS_LIVE, MFMediaType_Video,
    MFNominalRange_16_235, MFVideoFormat_NV12, MFVideoFormat_YUY2, MFVideoInterlace_Progressive,
    MFVideoPrimaries_BT709, MFVideoTransFunc_709, MFVideoTransferMatrix_BT709,
};
use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
use windows::Win32::System::Com::{IClassFactory, IClassFactory_Impl};
use windows::core::{BOOL, ComObject, GUID, HRESULT, IUnknown, IUnknownImpl, Interface, Ref, Result, implement, w};

use crate::com::{LiveGuard, lock_server, pack, set_not_found, unloadable};
use crate::format::{CLSID_U128, FPS, HEIGHT, PixelFormat, WIDTH};
use crate::stream::{MediaStream, StreamShared};

/// Name shown to apps; Windows appends "Windows Virtual Camera" to it.
const FRIENDLY_NAME: windows::core::PCWSTR = w!("Omuzaki spike camera");

/// A media type for `format` with the colour description a downstream converter needs (BT.709, limited range).
fn media_type(format: PixelFormat) -> Result<IMFMediaType> {
    // SAFETY: attribute setters on a freshly created media type, with valid GUID references.
    unsafe {
        let media_type = MFCreateMediaType()?;
        media_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        let subtype = match format {
            PixelFormat::Nv12 => &MFVideoFormat_NV12,
            PixelFormat::Yuy2 => &MFVideoFormat_YUY2,
        };
        media_type.SetGUID(&MF_MT_SUBTYPE, subtype)?;
        media_type.SetUINT64(&MF_MT_FRAME_SIZE, pack(WIDTH, HEIGHT))?;
        media_type.SetUINT64(&MF_MT_FRAME_RATE, pack(FPS, 1))?;
        media_type.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack(1, 1))?;
        media_type.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
        media_type.SetUINT32(&MF_MT_ALL_SAMPLES_INDEPENDENT, 1)?;
        media_type.SetUINT32(&MF_MT_DEFAULT_STRIDE, format.stride(WIDTH))?;
        let bytes = u32::try_from(format.frame_bytes(WIDTH, HEIGHT)).map_err(|_| windows::core::Error::from(E_FAIL))?;
        media_type.SetUINT32(&MF_MT_SAMPLE_SIZE, bytes)?;
        media_type.SetUINT32(&MF_MT_AVG_BITRATE, bytes * 8 * FPS)?;
        media_type.SetUINT32(&MF_MT_YUV_MATRIX, MFVideoTransferMatrix_BT709.0 as u32)?;
        media_type.SetUINT32(&MF_MT_VIDEO_NOMINAL_RANGE, MFNominalRange_16_235.0 as u32)?;
        media_type.SetUINT32(&MF_MT_VIDEO_PRIMARIES, MFVideoPrimaries_BT709.0 as u32)?;
        media_type.SetUINT32(&MF_MT_TRANSFER_FUNCTION, MFVideoTransFunc_709.0 as u32)?;
        Ok(media_type)
    }
}

/// The stream descriptor: NV12 first (the current type), YUY2 second, tagged as the capture pin of a Frame Server device.
fn stream_descriptor() -> Result<IMFStreamDescriptor> {
    let (nv12, yuy2) = (media_type(PixelFormat::Nv12)?, media_type(PixelFormat::Yuy2)?);
    // SAFETY: plain construction and attribute setting on objects created here.
    unsafe {
        let descriptor = MFCreateStreamDescriptor(0, &[Some(nv12.clone()), Some(yuy2)])?;
        descriptor.GetMediaTypeHandler()?.SetCurrentMediaType(&nv12)?;
        descriptor.SetGUID(&MF_DEVICESTREAM_STREAM_CATEGORY, &PINNAME_VIDEO_CAPTURE)?;
        descriptor.SetUINT32(&MF_DEVICESTREAM_STREAM_ID, 0)?;
        descriptor.SetUINT32(&MF_DEVICESTREAM_FRAMESERVER_SHARED, 1)?;
        Ok(descriptor)
    }
}

struct Inner {
    shutdown: bool,
    /// Created on the first `Start`; the same stream object is reused after that (`MEUpdatedStream`).
    stream: Option<(IMFMediaStream2, Arc<StreamShared>)>,
}

/// The camera as Media Foundation sees it.
#[implement(IMFMediaSourceEx, IMFGetService, IKsControl)]
pub struct MediaSource {
    events: IMFMediaEventQueue,
    descriptor: IMFStreamDescriptor,
    presentation: IMFPresentationDescriptor,
    attributes: IMFAttributes,
    inner: Mutex<Inner>,
    _live: LiveGuard,
}

impl MediaSource {
    fn create() -> Result<ComObject<Self>> {
        let descriptor = stream_descriptor()?;
        // SAFETY: plain construction.
        let (events, presentation, attributes) = unsafe {
            let events = MFCreateEventQueue()?;
            let presentation = MFCreatePresentationDescriptor(Some(&[Some(descriptor.clone())]))?;
            presentation.SelectStream(0)?;
            let mut attributes = None;
            MFCreateAttributes(&raw mut attributes, 2)?;
            let attributes: IMFAttributes = attributes.ok_or_else(|| windows::core::Error::from(E_FAIL))?;
            attributes.SetString(&MF_DEVSOURCE_ATTRIBUTE_FRIENDLY_NAME, FRIENDLY_NAME)?;
            attributes.SetGUID(&MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE, &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID)?;
            (events, presentation, attributes)
        };
        Ok(ComObject::new(Self {
            events,
            descriptor,
            presentation,
            attributes,
            inner: Mutex::new(Inner { shutdown: false, stream: None }),
            _live: LiveGuard::new(),
        }))
    }

    /// Fails with `MF_E_SHUTDOWN` once the source has been shut down.
    fn alive(&self) -> Result<()> {
        self.state().map(drop)
    }

    fn state(&self) -> Result<std::sync::MutexGuard<'_, Inner>> {
        let inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        if inner.shutdown { Err(MF_E_SHUTDOWN.into()) } else { Ok(inner) }
    }
}

impl Drop for MediaSource {
    fn drop(&mut self) {
        if let Some((_, shared)) = self.inner.get_mut().unwrap_or_else(PoisonError::into_inner).stream.take() {
            shared.shutdown();
        }
    }
}

impl IMFMediaEventGenerator_Impl for MediaSource_Impl {
    fn GetEvent(&self, flags: MEDIA_EVENT_GENERATOR_GET_EVENT_FLAGS) -> Result<IMFMediaEvent> {
        // GetEvent can block for as long as the consumer likes, so no lock is held while waiting.
        self.alive()?;
        // SAFETY: the queue is a valid COM object.
        unsafe { self.events.GetEvent(flags.0) }
    }

    fn BeginGetEvent(&self, callback: Ref<'_, IMFAsyncCallback>, state: Ref<'_, IUnknown>) -> Result<()> {
        self.alive()?;
        // SAFETY: the queue is a valid COM object; the references are valid for the call.
        unsafe { self.events.BeginGetEvent(callback.as_ref(), state.as_ref()) }
    }

    fn EndGetEvent(&self, result: Ref<'_, IMFAsyncResult>) -> Result<IMFMediaEvent> {
        self.alive()?;
        // SAFETY: as above.
        unsafe { self.events.EndGetEvent(result.as_ref()) }
    }

    fn QueueEvent(&self, kind: u32, extended: *const GUID, status: HRESULT, value: *const PROPVARIANT) -> Result<()> {
        self.alive()?;
        // SAFETY: the pointers come from the COM caller and are valid for the call.
        unsafe { self.events.QueueEventParamVar(kind, extended, status, value) }
    }
}

impl IMFMediaSource_Impl for MediaSource_Impl {
    fn GetCharacteristics(&self) -> Result<u32> {
        self.alive()?;
        // A live source with no seeking and no pausing, as Frame Server requires.
        Ok(MFMEDIASOURCE_IS_LIVE.0.cast_unsigned())
    }

    fn CreatePresentationDescriptor(&self) -> Result<IMFPresentationDescriptor> {
        self.alive()?;
        // A clone: the consumer picks the media type on its copy and passes it back to `Start`.
        // SAFETY: the descriptor is a valid COM object.
        unsafe { self.presentation.Clone() }
    }

    fn Start(
        &self,
        presentation: Ref<'_, IMFPresentationDescriptor>,
        time_format: *const GUID,
        start_position: *const PROPVARIANT,
    ) -> Result<()> {
        let presentation = presentation.ok()?;
        if start_position.is_null() {
            return Err(E_INVALIDARG.into());
        }
        // SAFETY: a non-null time-format pointer comes from the COM caller and points at a GUID.
        if !time_format.is_null() && unsafe { *time_format } != GUID::zeroed() {
            return Err(MF_E_UNSUPPORTED_TIME_FORMAT.into());
        }
        let mut inner = self.state()?;

        // Which stream and which of its media types the consumer chose.
        let mut selected = BOOL(0);
        let mut chosen = None;
        // SAFETY: out-pointers reference live locals.
        let format = unsafe {
            presentation.GetStreamDescriptorByIndex(0, &raw mut selected, &raw mut chosen)?;
            if !selected.as_bool() {
                return Err(MF_E_INVALIDREQUEST.into());
            }
            let subtype = chosen
                .ok_or_else(|| windows::core::Error::from(E_FAIL))?
                .GetMediaTypeHandler()?
                .GetCurrentMediaType()?
                .GetGUID(&MF_MT_SUBTYPE)?;
            if subtype == MFVideoFormat_YUY2 { PixelFormat::Yuy2 } else { PixelFormat::Nv12 }
        };

        let first_start = inner.stream.is_none();
        if first_start {
            let source: IMFMediaSource = self.to_interface::<IMFMediaSourceEx>().cast()?;
            let shared = StreamShared::spawn(self.descriptor.clone(), source)?;
            let stream: IMFMediaStream2 = ComObject::new(MediaStream::new(Arc::clone(&shared))).into_interface();
            inner.stream = Some((stream, shared));
        }
        let Some((stream, shared)) = inner.stream.as_ref() else { return Err(E_FAIL.into()) };

        // The stream goes to the consumer in MENewStream the first time and MEUpdatedStream afterwards, before
        // MESourceStarted: the order the Media Foundation pipeline relies on.
        let kind = if first_start { MENewStream } else { MEUpdatedStream };
        // SAFETY: the stream and queue are valid COM objects; the start position is the caller's valid pointer.
        unsafe {
            self.events.QueueEventParamUnk(kind.0.cast_unsigned(), &GUID::zeroed(), S_OK, stream)?;
            shared.start(format)?;
            self.events.QueueEventParamVar(MESourceStarted.0 as u32, &GUID::zeroed(), S_OK, start_position)
        }
    }

    fn Stop(&self) -> Result<()> {
        let inner = self.state()?;
        if let Some((_, shared)) = inner.stream.as_ref() {
            shared.stop()?;
        }
        let empty = PROPVARIANT::default();
        // SAFETY: `empty` is a valid VT_EMPTY value for the call.
        unsafe { self.events.QueueEventParamVar(MESourceStopped.0 as u32, &GUID::zeroed(), S_OK, &raw const empty) }
    }

    fn Pause(&self) -> Result<()> {
        // Frame Server sources cannot be paused (and must not post a pause event).
        self.alive()?;
        Err(MF_E_INVALID_STATE_TRANSITION.into())
    }

    fn Shutdown(&self) -> Result<()> {
        let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        if inner.shutdown {
            return Err(MF_E_SHUTDOWN.into());
        }
        inner.shutdown = true;
        if let Some((_, shared)) = inner.stream.take() {
            shared.shutdown();
        }
        // SAFETY: the queue is a valid COM object. Already-shut-down is not an error worth reporting.
        let _ = unsafe { self.events.Shutdown() };
        Ok(())
    }
}

impl IMFMediaSourceEx_Impl for MediaSource_Impl {
    fn GetSourceAttributes(&self) -> Result<IMFAttributes> {
        self.alive()?;
        Ok(self.attributes.clone())
    }

    fn GetStreamAttributes(&self, stream_id: u32) -> Result<IMFAttributes> {
        self.alive()?;
        if stream_id != 0 {
            return Err(E_INVALIDARG.into());
        }
        self.descriptor.cast()
    }

    fn SetD3DManager(&self, _manager: Ref<'_, IUnknown>) -> Result<()> {
        // Frames are made on the CPU, so a Direct3D manager is of no use; ignoring it is allowed.
        self.alive()?;
        Ok(())
    }
}

impl IMFGetService_Impl for MediaSource_Impl {
    fn GetService(&self, _service: *const GUID, _riid: *const GUID, object: *mut *mut c_void) -> Result<()> {
        self.alive()?;
        if object.is_null() {
            return Err(E_POINTER.into());
        }
        // SAFETY: `object` is the caller's out-pointer.
        unsafe { *object = std::ptr::null_mut() };
        // No service interfaces are offered, which Frame Server documents as acceptable.
        Err(MF_E_UNSUPPORTED_SERVICE.into())
    }
}

impl IKsControl_Impl for MediaSource_Impl {
    fn KsProperty(&self, _: *const KSIDENTIFIER, _: u32, _: *mut c_void, _: u32, _: *mut u32) -> Result<()> {
        Err(set_not_found())
    }

    fn KsMethod(&self, _: *const KSIDENTIFIER, _: u32, _: *mut c_void, _: u32, _: *mut u32) -> Result<()> {
        Err(set_not_found())
    }

    fn KsEvent(&self, _: *const KSIDENTIFIER, _: u32, _: *mut c_void, _: u32, _: *mut u32) -> Result<()> {
        Err(set_not_found())
    }
}

/// Makes [`MediaSource`] objects for COM.
#[implement(IClassFactory)]
struct ClassFactory;

impl IClassFactory_Impl for ClassFactory_Impl {
    fn CreateInstance(&self, outer: Ref<'_, IUnknown>, riid: *const GUID, object: *mut *mut c_void) -> Result<()> {
        if object.is_null() || riid.is_null() {
            return Err(E_POINTER.into());
        }
        if outer.is_some() {
            return Err(CLASS_E_NOAGGREGATION.into());
        }
        let source: IMFMediaSourceEx = MediaSource::create()?.into_interface();
        // SAFETY: both pointers were checked and come from the COM caller.
        unsafe { source.query(riid, object).ok() }
    }

    fn LockServer(&self, lock: BOOL) -> Result<()> {
        lock_server(lock.as_bool());
        Ok(())
    }
}

/// COM entry point: hands out the class factory for this camera's class id.
///
/// # Safety
/// Called by COM with valid pointers (or null, which is rejected).
#[unsafe(no_mangle)]
pub unsafe extern "system" fn DllGetClassObject(
    clsid: *const GUID,
    riid: *const GUID,
    object: *mut *mut c_void,
) -> HRESULT {
    if clsid.is_null() || riid.is_null() || object.is_null() {
        return E_POINTER;
    }
    // SAFETY: checked non-null above; COM passes valid GUID and out pointers.
    unsafe {
        *object = std::ptr::null_mut();
        if *clsid != GUID::from_u128(CLSID_U128) {
            return CLASS_E_CLASSNOTAVAILABLE;
        }
        let factory: IClassFactory = ClassFactory.into();
        factory.query(riid, object)
    }
}

/// COM entry point: whether the DLL may be unloaded (no objects or locks outstanding).
#[unsafe(no_mangle)]
pub extern "system" fn DllCanUnloadNow() -> HRESULT {
    if unloadable() { S_OK } else { S_FALSE }
}
