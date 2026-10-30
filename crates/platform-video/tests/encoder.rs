//! The encoder writes a valid MP4 (boxes, codecs, frame and sound counts)
//! and keeps picture and sound in step. Windows only (Media Foundation).
#![cfg(windows)]
use platform_video::{mp4, onset, PixelLayout, Recorder, SegmentFormat, SourceSpec};
use std::path::{Path, PathBuf};

fn out(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("platform-video");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(name);
    let _ = std::fs::remove_file(&path);
    let _ = std::fs::remove_file(platform_video::segment_sidecar(&path));
    path
}

/// 90 pictures of a 512x512 4:3 source; picture 45 is white (the rest
/// dark grey) and a 1 kHz tone starts exactly at its sound frame.
fn write_clip(path: &Path) -> (SegmentFormat, u64, u64) {
    let format = SegmentFormat::DEFAULT;
    let source = SourceSpec {
        width: 512,
        height: 512,
        layout: PixelLayout::Rgba,
        display_aspect: (4, 3),
    };
    let mut rec = Recorder::start(path, format, source, "platform-video test").unwrap();
    let spf = format.samples_per_frame() as usize;
    let (frames, flash) = (90u64, 45u64);
    for n in 0..frames {
        let level = if n == flash { 255 } else { 40 };
        rec.frame(vec![level; 512 * 512 * 4]).unwrap();
        let mut pcm = vec![0i16; spf * 2];
        if n >= flash {
            for (i, s) in pcm.chunks_exact_mut(2).enumerate() {
                let t = (n as usize * spf + i) as f64 / 48_000.0;
                let v = (t * 1000.0 * std::f64::consts::TAU).sin() * 12_000.0;
                s[0] = v as i16;
                s[1] = v as i16;
            }
        }
        rec.audio(pcm).unwrap();
    }
    let summary = rec.finish().unwrap();
    assert_eq!(summary.frames, frames);
    assert_eq!(summary.audio_frames, frames * spf as u64);
    (format, frames, flash)
}

#[test]
fn writes_a_valid_short_mp4_in_sync() {
    let path = out("encoder-test.mp4");
    let (format, frames, flash) = write_clip(&path);

    // The boxes: ftyp, moov and mdat; an avc1 track and an mp4a track.
    let info = mp4::inspect(&path).unwrap();
    for b in ["ftyp", "moov", "mdat"] {
        assert!(
            info.boxes.iter().any(|x| x == b),
            "missing {b}: {:?}",
            info.boxes
        );
    }
    let video = info.track("vide").expect("video track");
    assert_eq!(video.codec, "avc1");
    assert_eq!(video.samples, frames);
    assert_eq!((video.width, video.height), (1920, 1080));
    let seconds = video.duration as f64 / f64::from(video.timescale);
    assert!((seconds - 1.5).abs() < 0.02, "video lasts {seconds} s");
    let audio = info.track("soun").expect("sound track");
    assert_eq!(audio.codec, "mp4a");
    assert_eq!(audio.timescale, 48_000);

    // The sidecar.
    let side: platform_video::SegmentInfo =
        serde_json::from_slice(&std::fs::read(platform_video::segment_sidecar(&path)).unwrap())
            .unwrap();
    assert_eq!(side.format, format);
    assert_eq!((side.frames, side.audio_frames), (frames, frames * 800));
    assert_eq!(side.picture_box, [240, 0, 1440, 1080]);

    // Read back through Media Foundation.
    let p = platform_video::probe(&path).unwrap();
    assert_eq!((p.width, p.height, p.fps), (1920, 1080, (60, 1)));
    assert_eq!(p.video_frames, frames);
    eprintln!(
        "compressed pictures: start {} end {}",
        p.video_start, p.video_end
    );
    assert!(
        (p.video_end - p.video_start - format.frame_time(frames)).abs() <= 1,
        "video {}..{}",
        p.video_start,
        p.video_end
    );
    assert_eq!((p.audio_rate, p.audio_channels), (48_000, 2));
    let audio_s = p.audio_frames as f64 / 48_000.0;
    assert!((audio_s - 1.5).abs() < 0.05, "sound lasts {audio_s} s");

    // Sync: the white picture and the tone's onset within one frame.
    let mut white = None;
    let n = platform_video::decode_video_luma(&path, |i, ts, luma, stride| {
        let centre = luma[540 * stride + 960];
        let bar = luma[540 * stride + 100];
        assert!(bar < 24, "bars must stay black");
        if centre > 200 && white.is_none() {
            white = Some((i, ts));
        }
    })
    .unwrap();
    assert_eq!(n, frames);
    let (index, ts) = white.expect("the white picture");
    assert_eq!(index, flash);
    let a = platform_video::decode_audio(&path).unwrap();
    let at = onset(&a.samples, 0, 3000).expect("the tone");
    let tone = a.start + (at as i64 * 10_000_000 / 48_000);
    let frame = format.frame_time(1);
    assert!(
        (tone - ts).abs() <= frame,
        "tone at {tone}, picture at {ts} (100 ns)"
    );
    eprintln!(
        "sync: picture {ts}, tone {tone}, difference {} ms",
        (tone - ts) as f64 / 10_000.0
    );
}

#[test]
fn refuses_existing_output_and_bad_frames() {
    let path = out("encoder-refuse.mp4");
    std::fs::write(&path, b"x").unwrap();
    let source = SourceSpec::square(64, 64, PixelLayout::Bgrx);
    assert!(Recorder::start(&path, SegmentFormat::DEFAULT, source, "t").is_err());
    std::fs::remove_file(&path).unwrap();
    let mut rec = Recorder::start(&path, SegmentFormat::DEFAULT, source, "t").unwrap();
    assert!(rec.frame(vec![0; 10]).is_err());
    rec.frame(vec![0; 64 * 64 * 4]).unwrap();
    assert_eq!(rec.finish().unwrap().frames, 1);
}
