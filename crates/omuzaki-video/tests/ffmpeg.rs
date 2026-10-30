//! Reading videos through ffmpeg: the real program on the formats people record in, when it is installed, and stand-in
//! programs that check what Omuzaki asks of it and how it reports a failure, which run everywhere.
#![allow(clippy::disallowed_methods)] // tests write their own clips and stand-in programs into a temporary folder
#![allow(clippy::unwrap_used)] // a panic in a helper is a test failure

use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Instant;

use omuzaki_video::ffmpeg::Ffmpeg;
use omuzaki_video::{Picture, VideoError, VideoReader};

/// Whether the real ffmpeg runs here.
fn have_ffmpeg() -> bool {
    Command::new("ffmpeg").arg("-version").output().is_ok_and(|o| o.status.success())
}

/// Encodes one second of ffmpeg's test pattern at `size` into `dir/name` with `codec`, or `None` when this ffmpeg
/// lacks the encoder.
fn encode(dir: &Path, name: &str, size: &str, codec: &[&str]) -> Option<PathBuf> {
    let out = dir.join(name);
    let ok = Command::new("ffmpeg")
        .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "lavfi", "-i"])
        .arg(format!("testsrc2=size={size}:rate=30:duration=1"))
        .args(codec)
        .arg(&out)
        .status()
        .is_ok_and(|s| s.success());
    ok.then_some(out)
}

/// Reads every frame of `path`, returning the count.
fn count(path: &Path) -> (omuzaki_video::VideoInfo, usize) {
    let mut reader = VideoReader::open(path, &Ffmpeg::default()).unwrap();
    let mut picture = Picture::default();
    let mut frames = 0;
    while reader.read(&mut picture).unwrap() {
        assert_eq!((picture.width, picture.height), (reader.info().width, reader.info().height));
        frames += 1;
    }
    (reader.info(), frames)
}

#[test]
fn the_formats_people_record_in_decode_through_ffmpeg() {
    if !have_ffmpeg() {
        eprintln!("skipped: no ffmpeg on this machine");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let cases: [(&str, &str, &[&str]); 4] = [
        ("phone.mp4", "640x360", &["-c:v", "libx264", "-pix_fmt", "yuv420p"]),
        ("obs.mkv", "640x360", &["-c:v", "libx264"]),
        ("clip.webm", "640x360", &["-c:v", "libvpx-vp9", "-b:v", "1M"]),
        ("clip.mov", "640x360", &["-c:v", "mpeg4"]),
    ];
    for (name, size, codec) in cases {
        let Some(path) = encode(dir.path(), name, size, codec) else {
            eprintln!("skipped {name}: this ffmpeg has no such encoder");
            continue;
        };
        let begun = Instant::now();
        let (info, frames) = count(&path);
        assert_eq!((info.width, info.height, info.fps_num, info.fps_den, frames), (640, 360, 30, 1, 30), "{name}");
        eprintln!("{name}: 30 frames in {:?}", begun.elapsed());
    }
}

#[test]
fn a_4k_video_decodes_at_the_long_side_limit() {
    if !have_ffmpeg() {
        eprintln!("skipped: no ffmpeg on this machine");
        return;
    }
    let dir = tempfile::tempdir().unwrap();
    let Some(path) = encode(dir.path(), "4k.mp4", "3840x2160", &["-c:v", "mpeg4", "-frames:v", "3"]) else { return };
    let (info, frames) = count(&path);
    assert_eq!((info.width, info.height, frames), (1280, 720, 3));
    // A portrait phone video keeps its shape.
    let Some(path) = encode(dir.path(), "tall.mp4", "1080x1920", &["-c:v", "mpeg4", "-frames:v", "2"]) else { return };
    let (info, _) = count(&path);
    assert_eq!((info.width, info.height), (720, 1280));
}

#[test]
fn a_missing_program_says_how_to_get_one() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("clip.mp4");
    std::fs::write(&path, b"not a video").unwrap();
    let error = VideoReader::open(&path, &Ffmpeg::new(dir.path().join("no-ffmpeg-here"))).unwrap_err();
    assert_eq!(error, VideoError::NoFfmpeg);
    assert!(error.to_string().contains("ffmpeg.org"), "{error}");
    assert!(matches!(VideoReader::open(&dir.path().join("gone.mp4"), &Ffmpeg::default()), Err(VideoError::Missing(_))));
}

/// Stand-in programs: shell scripts, so these run where `sh` does.
#[cfg(unix)]
mod stand_in {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    /// A program at `dir/name` running `script`.
    fn program(dir: &Path, name: &str, script: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{script}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[test]
    fn ffmpeg_is_asked_for_local_files_only_and_the_first_video_stream() {
        let dir = tempfile::tempdir().unwrap();
        let video = dir.path().join("-looks like an option.mp4");
        std::fs::write(&video, b"anything").unwrap();
        let args = dir.path().join("args");
        // Records its arguments, then writes a one-frame 4x2 Y4M stream.
        let fake = program(
            dir.path(),
            "ffmpeg",
            &format!(
                "printf '%s\\n' \"$@\" > '{}'\nprintf 'YUV4MPEG2 W4 H2 F25:1 C420jpeg\\nFRAME\\n'\nprintf 'abcdefghxyzw'",
                args.display()
            ),
        );
        let mut reader = VideoReader::open(&video, &Ffmpeg::new(&fake)).unwrap();
        assert_eq!((reader.info().width, reader.info().height, reader.info().fps()), (4, 2, 25.0));
        let mut picture = Picture::default();
        assert!(reader.read(&mut picture).unwrap());
        assert_eq!(&picture.data[..8], b"abcdefgh");
        assert_eq!(&picture.data[8..12], b"xzyw", "U and V interleave into NV12");
        assert!(!reader.read(&mut picture).unwrap());
        let said = std::fs::read_to_string(&args).unwrap();
        let said: Vec<&str> = said.lines().collect();
        let input = said.iter().position(|a| *a == "-i").map(|i| said[i + 1]).unwrap();
        assert_eq!(
            input,
            format!("file:{}", video.canonicalize().unwrap().display()),
            "a path never reads as an option"
        );
        let whitelist = said.iter().position(|a| *a == "-protocol_whitelist").map(|i| said[i + 1]);
        assert_eq!(whitelist, Some("file"), "no network protocol: {said:?}");
        assert!(said.windows(2).any(|w| w == ["-map", "0:v:0"]), "{said:?}");
        assert!(said.contains(&"-nostdin") && said.contains(&"-an"), "{said:?}");
    }

    #[test]
    fn ffmpegs_own_words_explain_a_file_it_cannot_read() {
        let dir = tempfile::tempdir().unwrap();
        let video = dir.path().join("broken.mp4");
        std::fs::write(&video, b"anything").unwrap();
        let fake = program(dir.path(), "ffmpeg", "echo 'moov atom not found' >&2\nexit 1");
        let error = VideoReader::open(&video, &Ffmpeg::new(&fake)).unwrap_err();
        assert_eq!(error, VideoError::Decode("moov atom not found".into()));
    }

    #[test]
    fn a_decode_that_fails_partway_is_an_error_not_an_end() {
        let dir = tempfile::tempdir().unwrap();
        let video = dir.path().join("cut.mp4");
        std::fs::write(&video, b"anything").unwrap();
        let fake = program(
            dir.path(),
            "ffmpeg",
            "printf 'YUV4MPEG2 W4 H2 F25:1\\nFRAME\\nabcdefghxyzw'\necho 'Invalid data found' >&2\nexit 1",
        );
        let mut reader = VideoReader::open(&video, &Ffmpeg::new(&fake)).unwrap();
        let mut picture = Picture::default();
        assert!(reader.read(&mut picture).unwrap());
        assert_eq!(reader.read(&mut picture), Err(VideoError::Decode("Invalid data found".into())));
    }
}
