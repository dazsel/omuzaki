//! The mode, subtype and error tables (they build and run on every platform).

use omuzaki_camera_mf::modes::{
    E_ACCESSDENIED, MF_E_HW_MFT_FAILED_START_STREAMING, MF_E_VIDEO_RECORDING_DEVICE_INVALIDATED, MJPG, ModeInfo, NV12,
    RGB32, YUY2, choose_mode, map_hresult, subtype_format,
};
use omuzaki_track_api::camera::{CameraFormat, CameraFormatRequest, CameraId, PixelFormat};
use omuzaki_track_api::error::TrackError;

const fn mode(format: PixelFormat, width: u32, height: u32, fps: u32) -> ModeInfo {
    ModeInfo { format, width, height, fps_num: fps, fps_den: 1 }
}

/// Twelve modes: three layouts at 1280x720 (30 and 60 fps), 640x480 and 1920x1080.
fn table() -> Vec<ModeInfo> {
    use PixelFormat::{Mjpg, Nv12, Yuy2};
    vec![
        mode(Mjpg, 1280, 720, 30),
        mode(Yuy2, 1280, 720, 30),
        mode(Nv12, 1280, 720, 30),
        mode(Mjpg, 1280, 720, 60),
        mode(Yuy2, 1280, 720, 60),
        mode(Nv12, 1280, 720, 60),
        mode(Mjpg, 640, 480, 30),
        mode(Yuy2, 640, 480, 30),
        mode(Nv12, 640, 480, 30),
        mode(Mjpg, 1920, 1080, 30),
        mode(Yuy2, 1920, 1080, 30),
        mode(Nv12, 1920, 1080, 30),
    ]
}

#[test]
fn nv12_beats_yuy2_beats_mjpg_at_the_requested_size() {
    let request = CameraFormatRequest::new(1280, 720, 30);
    let mut modes = table();
    assert_eq!(choose_mode(&modes, &request), Some(mode(PixelFormat::Nv12, 1280, 720, 30)));
    modes.retain(|m| m.format != PixelFormat::Nv12);
    assert_eq!(choose_mode(&modes, &request), Some(mode(PixelFormat::Yuy2, 1280, 720, 30)));
    modes.retain(|m| m.format != PixelFormat::Yuy2);
    assert_eq!(choose_mode(&modes, &request), Some(mode(PixelFormat::Mjpg, 1280, 720, 30)));
}

#[test]
fn sixty_fps_is_not_chosen_over_thirty_for_a_thirty_request() {
    let picked = choose_mode(&table(), &CameraFormatRequest::new(1280, 720, 30)).unwrap();
    assert_eq!(picked.to_format().fps, 30);
    let picked = choose_mode(&table(), &CameraFormatRequest::new(1280, 720, 60)).unwrap();
    assert_eq!(picked.to_format().fps, 60);
}

#[test]
fn a_camera_with_one_size_gets_it() {
    let only = [mode(PixelFormat::Yuy2, 640, 480, 30)];
    assert_eq!(choose_mode(&only, &CameraFormatRequest::new(1280, 720, 30)), Some(only[0]));
}

#[test]
fn no_modes_or_no_matching_layout_picks_nothing() {
    let request = CameraFormatRequest::new(1280, 720, 30);
    assert_eq!(choose_mode(&[], &request), None);
    let request = request.with_pixel(Some(PixelFormat::Bgra8));
    assert_eq!(choose_mode(&table(), &request), None);
}

#[test]
fn fractional_rates_round_to_whole_frames() {
    let ntsc = ModeInfo { format: PixelFormat::Nv12, width: 1280, height: 720, fps_num: 30_000, fps_den: 1001 };
    assert_eq!(ntsc.to_format(), CameraFormat::new(1280, 720, 30, PixelFormat::Nv12));
    let none = ModeInfo { fps_den: 0, ..ntsc };
    assert_eq!(none.to_format().fps, 0);
}

#[test]
fn subtypes_map_to_layouts() {
    assert_eq!(subtype_format(NV12), Some(PixelFormat::Nv12));
    assert_eq!(subtype_format(YUY2), Some(PixelFormat::Yuy2));
    assert_eq!(subtype_format(MJPG), Some(PixelFormat::Mjpg));
    assert_eq!(subtype_format(RGB32), Some(PixelFormat::Bgra8));
    assert_eq!(subtype_format(20), None, "RGB24 is B, G, R in Media Foundation; Rgb8 is R, G, B");
    assert_eq!(subtype_format(u32::from_le_bytes(*b"H264")), None);
    assert_eq!(NV12, 0x3231_564E);
}

#[test]
fn hresults_map_to_track_errors() {
    let id = CameraId("cam".into());
    assert_eq!(map_hresult(E_ACCESSDENIED, &id), TrackError::PermissionDenied);
    assert_eq!(map_hresult(MF_E_VIDEO_RECORDING_DEVICE_INVALIDATED, &id), TrackError::Lost);
    assert_eq!(map_hresult(MF_E_HW_MFT_FAILED_START_STREAMING, &id), TrackError::Busy(id.clone()));
    assert_eq!(
        map_hresult(0x8000_4005_u32.cast_signed(), &id),
        TrackError::Backend("Media Foundation error 0x80004005".into())
    );
}
