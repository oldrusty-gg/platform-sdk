//! Video recording: a tool for making videos of the games, not gameplay
//! (docs/ADDITIONS.md). Rendered frames and the game's own mixed sound go
//! straight into an H.264/AAC MP4 through Windows Media Foundation's Sink
//! Writer (`MFCreateSinkWriterFromURL`), which is part of Windows: no ffmpeg
//! and no other program.
//!
//! - [`SegmentFormat`]: every clip ("segment") an editor stitches has the
//!   same size, frame rate and sound format (1920x1080, 60 fps, AAC 48 kHz
//!   stereo by default). A `<clip>.mp4.segment.json` sidecar records the
//!   format and the exact frame and sound-frame counts.
//! - [`Letterbox`]: the source frame scaled by whole pixels (nearest) into
//!   the frame, keeping its display aspect (the games' 4:3), black bars
//!   around it, converted to NV12 (BT.709, limited range).
//! - `Recorder` (Windows): the encoder on its own thread. Frame `n` is
//!   stamped `n / fps`; sound by the number of sound frames written, so
//!   picture and sound stay exactly in step when the caller hands over
//!   `sample_rate / fps` sound frames per picture.
//! - [`mp4`]: a small box reader used by the tests (brands, tracks, codecs,
//!   sample counts).
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[cfg(windows)]
mod mf;
pub mod mp4;
#[cfg(windows)]
pub use mf::{
    decode_audio, decode_video, decode_video_luma, probe, DecodedAudio, Probe, Recorder, Summary,
};

/// The first sound frame at or after `from` whose level (either channel)
/// reaches `threshold`: a sound's onset in decoded audio.
pub fn onset(stereo: &[i16], from: usize, threshold: i16) -> Option<usize> {
    stereo
        .chunks_exact(2)
        .enumerate()
        .skip(from)
        .find(|(_, s)| {
            s[0].unsigned_abs() >= threshold.unsigned_abs()
                || s[1].unsigned_abs() >= threshold.unsigned_abs()
        })
        .map(|(i, _)| i)
}

/// The sidecar's schema.
pub const SEGMENT_SCHEMA: &str = "roses-video-segment-1";

/// The format every clip of one video shares.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SegmentFormat {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    /// Sound frames per second (48 kHz: the games' mixer rate).
    pub sample_rate: u32,
    pub channels: u16,
    /// H.264 High profile, bits per second.
    pub video_bitrate: u32,
    /// AAC-LC, bits per second (Media Foundation's AAC encoder takes 96,
    /// 128, 160 or 192 kbit/s).
    pub audio_bitrate: u32,
}

impl Default for SegmentFormat {
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl SegmentFormat {
    /// 1080p60, H.264 16 Mbit/s, AAC 192 kbit/s, 48 kHz stereo.
    pub const DEFAULT: Self = Self {
        width: 1920,
        height: 1080,
        fps: 60,
        sample_rate: 48_000,
        channels: 2,
        video_bitrate: 16_000_000,
        audio_bitrate: 192_000,
    };

    /// The default format at another size or frame rate. Sizes are even
    /// (NV12) and 64..=4096; the frame rate divides 48000 so every frame has
    /// a whole number of sound frames (24, 25, 30, 50, 60, ...).
    pub fn new(width: u32, height: u32, fps: u32) -> Result<Self, String> {
        let format = Self {
            width,
            height,
            fps,
            ..Self::DEFAULT
        };
        format.validate()?;
        Ok(format)
    }

    pub fn validate(&self) -> Result<(), String> {
        let size_ok = |v: u32| (64..=4096).contains(&v) && v.is_multiple_of(2);
        if !size_ok(self.width) || !size_ok(self.height) {
            return Err(format!(
                "recording size {}x{} must be even and 64..=4096",
                self.width, self.height
            ));
        }
        if self.fps == 0 || self.fps > 120 || !self.sample_rate.is_multiple_of(self.fps) {
            return Err(format!(
                "recording frame rate {} must be 1..=120 and divide {} (24, 25, 30, 50, 60 ...)",
                self.fps, self.sample_rate
            ));
        }
        if self.channels != 2 || self.sample_rate != 48_000 {
            return Err("recordings use 48 kHz stereo sound".into());
        }
        if ![96_000, 128_000, 160_000, 192_000].contains(&self.audio_bitrate) {
            return Err("AAC bitrate must be 96, 128, 160 or 192 kbit/s".into());
        }
        Ok(())
    }

    /// `1920x1080`.
    pub fn parse_size(text: &str) -> Result<(u32, u32), String> {
        let (w, h) = text
            .split_once(['x', 'X'])
            .ok_or_else(|| format!("size {text:?} is not WIDTHxHEIGHT"))?;
        let parse = |v: &str| {
            v.trim()
                .parse::<u32>()
                .map_err(|_| format!("size {text:?} is not WIDTHxHEIGHT"))
        };
        Ok((parse(w)?, parse(h)?))
    }

    /// Sound frames (one sample per channel) per picture.
    pub fn samples_per_frame(&self) -> u32 {
        self.sample_rate / self.fps
    }

    /// Start of picture `n` in 100 ns units (Media Foundation's clock).
    pub fn frame_time(&self, n: u64) -> i64 {
        (u128::from(n) * 10_000_000 / u128::from(self.fps)) as i64
    }

    /// Start of sound frame `n` in 100 ns units.
    pub fn sample_time(&self, n: u64) -> i64 {
        (u128::from(n) * 10_000_000 / u128::from(self.sample_rate)) as i64
    }
}

/// Byte order of a source frame's 4-byte pixels.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PixelLayout {
    /// R, G, B, A (Bevy's readback).
    Rgba,
    /// B, G, R, x (a GDI DIB's 0x00RRGGBB words, little-endian).
    Bgrx,
}

/// What the recorder is given each frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SourceSpec {
    pub width: u32,
    pub height: u32,
    pub layout: PixelLayout,
    /// The shape the source is shown at: (4, 3) for the games' 512x512
    /// framebuffers, (width, height) for square pixels.
    pub display_aspect: (u32, u32),
}

impl SourceSpec {
    pub fn square(width: u32, height: u32, layout: PixelLayout) -> Self {
        Self {
            width,
            height,
            layout,
            display_aspect: (width, height),
        }
    }

    fn square_pixels(&self) -> bool {
        u64::from(self.width) * u64::from(self.display_aspect.1)
            == u64::from(self.height) * u64::from(self.display_aspect.0)
    }
}

/// The picture's place in the frame and the nearest-pixel maps.
#[derive(Clone, Debug)]
pub struct Letterbox {
    pub source: SourceSpec,
    pub width: u32,
    pub height: u32,
    /// The picture's box inside the frame (even values).
    pub x: u32,
    pub y: u32,
    pub box_width: u32,
    pub box_height: u32,
    xmap: Vec<u32>,
    ymap: Vec<u32>,
}

impl Letterbox {
    /// Square-pixel sources use the largest whole-number scale that fits
    /// (1:1 when they already fit); others (the games' 4:3 framebuffers)
    /// fill the largest box of their display aspect, nearest pixel.
    pub fn new(source: SourceSpec, width: u32, height: u32) -> Self {
        let even = |v: u32| v & !1;
        let (sw, sh) = (source.width.max(1), source.height.max(1));
        let (bw, bh) = match (source.square_pixels(), (width / sw).min(height / sh)) {
            (true, k) if k >= 1 => (sw * k, sh * k),
            _ => {
                let (an, ad) = (
                    u64::from(source.display_aspect.0.max(1)),
                    u64::from(source.display_aspect.1.max(1)),
                );
                let wide = u64::from(height) * an / ad;
                if wide <= u64::from(width) {
                    (wide as u32, height)
                } else {
                    (width, (u64::from(width) * ad / an) as u32)
                }
            }
        };
        let (bw, bh) = (even(bw.min(width)).max(2), even(bh.min(height)).max(2));
        let x = even((width - bw) / 2);
        let y = even((height - bh) / 2);
        // Pixel centres: output pixel i samples source floor((i + 0.5) * s / b).
        let map = |b: u32, s: u32| {
            (0..b)
                .map(|i| ((u64::from(i) * 2 + 1) * u64::from(s) / (u64::from(b) * 2)) as u32)
                .collect()
        };
        Self {
            source,
            width,
            height,
            x,
            y,
            box_width: bw,
            box_height: bh,
            xmap: map(bw, sw),
            ymap: map(bh, sh),
        }
    }

    /// NV12 bytes for one frame (`width * height * 3 / 2`).
    pub fn nv12_len(&self) -> usize {
        self.width as usize * self.height as usize * 3 / 2
    }

    /// Scale `pixels` (the source, 4 bytes a pixel) into `out` as NV12:
    /// BT.709 limited range, chroma the mean of each 2x2 block.
    pub fn to_nv12(&self, pixels: &[u8], out: &mut Vec<u8>) -> Result<(), String> {
        let (sw, sh) = (self.source.width as usize, self.source.height as usize);
        if pixels.len() != sw * sh * 4 {
            return Err(format!(
                "frame has {} bytes; {sw}x{sh} needs {}",
                pixels.len(),
                sw * sh * 4
            ));
        }
        let (w, h) = (self.width as usize, self.height as usize);
        out.clear();
        out.resize(w * h, 16);
        out.resize(w * h * 3 / 2, 128);
        let (luma, chroma) = out.split_at_mut(w * h);
        let (ri, bi) = match self.source.layout {
            PixelLayout::Rgba => (0, 2),
            PixelLayout::Bgrx => (2, 0),
        };
        let (x0, y0) = (self.x as usize, self.y as usize);
        let mut row = vec![[0i32; 3]; self.box_width as usize];
        for by in (0..self.box_height as usize).step_by(2) {
            let mut sum = vec![[0i32; 3]; self.box_width as usize / 2];
            for dy in 0..2 {
                let src = &pixels[self.ymap[by + dy] as usize * sw * 4..][..sw * 4];
                for (bx, rgb) in row.iter_mut().enumerate() {
                    let p = &src[self.xmap[bx] as usize * 4..][..4];
                    *rgb = [i32::from(p[ri]), i32::from(p[1]), i32::from(p[bi])];
                }
                let line = &mut luma[(y0 + by + dy) * w + x0..][..self.box_width as usize];
                for (bx, &[r, g, b]) in row.iter().enumerate() {
                    // 219/255 * (0.2126, 0.7152, 0.0722), x 65536.
                    line[bx] = (16 + ((11966 * r + 40254 * g + 4064 * b + 32768) >> 16)) as u8;
                    let s = &mut sum[bx / 2];
                    s[0] += r;
                    s[1] += g;
                    s[2] += b;
                }
            }
            let line = &mut chroma[(y0 + by) / 2 * w + x0..][..self.box_width as usize];
            for (i, &[r, g, b]) in sum.iter().enumerate() {
                // 224/255 * BT.709 Cb/Cr rows, x 65536, over the 4-pixel sum.
                let cb = (-6596 * r - 22189 * g + 28785 * b) / 4;
                let cr = (28785 * r - 26145 * g - 2640 * b) / 4;
                line[i * 2] = (128 + ((cb + 32768) >> 16)).clamp(16, 240) as u8;
                line[i * 2 + 1] = (128 + ((cr + 32768) >> 16)).clamp(16, 240) as u8;
            }
        }
        Ok(())
    }
}

/// The sidecar an editor (or a stitching script) reads next to each clip.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct SegmentInfo {
    pub schema: String,
    pub format: SegmentFormat,
    /// Who recorded it (`roses-bevy-scene`, `roses-launcher`).
    pub source: String,
    /// Source frame size and the box it fills in the frame.
    pub source_size: [u32; 2],
    pub picture_box: [u32; 4],
    pub frames: u64,
    pub audio_frames: u64,
    pub duration_seconds: f64,
}

/// `<clip>.mp4.segment.json`.
pub fn segment_sidecar(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".segment.json");
    PathBuf::from(name)
}

/// A recording's output must be a new `.mp4` in an existing folder.
pub fn check_output(path: &Path) -> Result<(), String> {
    if path
        .extension()
        .and_then(|v| v.to_str())
        .is_none_or(|v| !v.eq_ignore_ascii_case("mp4"))
    {
        return Err(format!(
            "recording output {} must have a .mp4 extension",
            path.display()
        ));
    }
    if path.exists() {
        return Err(format!(
            "recording output already exists: {}",
            path.display()
        ));
    }
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    if !parent.is_dir() {
        return Err(format!(
            "recording output folder does not exist: {}",
            parent.display()
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_validation_and_clock() {
        let f = SegmentFormat::DEFAULT;
        assert!(f.validate().is_ok());
        assert_eq!(f.samples_per_frame(), 800);
        assert_eq!(f.frame_time(60), 10_000_000);
        assert_eq!(f.frame_time(1), 166_666);
        assert_eq!(f.sample_time(48_000), 10_000_000);
        // Frame n's sound starts exactly where frame n starts.
        for n in [0u64, 1, 7, 599, 3600] {
            assert_eq!(f.frame_time(n), f.sample_time(n * 800));
        }
        assert!(SegmentFormat::new(1920, 1080, 25).is_ok());
        assert!(SegmentFormat::new(1920, 1080, 29).is_err());
        assert!(SegmentFormat::new(1921, 1080, 60).is_err());
        assert_eq!(SegmentFormat::parse_size("1280x720"), Ok((1280, 720)));
        assert!(SegmentFormat::parse_size("1280").is_err());
    }

    #[test]
    fn game_framebuffer_fills_a_4x3_box_with_bars() {
        let src = SourceSpec {
            width: 512,
            height: 512,
            layout: PixelLayout::Rgba,
            display_aspect: (4, 3),
        };
        let b = Letterbox::new(src, 1920, 1080);
        assert_eq!((b.x, b.y, b.box_width, b.box_height), (240, 0, 1440, 1080));
        // Nearest pixel: every source column and row is used, none skipped.
        assert_eq!(b.xmap[0], 0);
        assert_eq!(*b.xmap.last().unwrap(), 511);
        let mut seen = vec![false; 512];
        b.ymap.iter().for_each(|&y| seen[y as usize] = true);
        assert!(seen.iter().all(|&s| s));
        // White picture: bars stay black (Y 16), the box is Y 235, grey chroma.
        let mut out = Vec::new();
        b.to_nv12(&vec![255; 512 * 512 * 4], &mut out).unwrap();
        assert_eq!(out.len(), 1920 * 1080 * 3 / 2);
        assert_eq!(out[0], 16);
        assert_eq!(out[240], 235);
        assert_eq!(out[1919], 16);
        assert_eq!(out[1920 * 1080 + 240], 128);
        // Pure red: BT.709 limited range Y 63, Cb 102, Cr 240.
        let red: Vec<u8> = (0..512 * 512).flat_map(|_| [255, 0, 0, 255]).collect();
        b.to_nv12(&red, &mut out).unwrap();
        assert_eq!(out[1920 * 500 + 1000], 63);
        let c = 1920 * 1080 + 250 * 1920 + 1000;
        assert_eq!((out[c], out[c + 1]), (102, 240));
    }

    #[test]
    fn square_pixels_use_whole_number_scale() {
        let b = Letterbox::new(
            SourceSpec::square(1800, 1080, PixelLayout::Bgrx),
            1920,
            1080,
        );
        assert_eq!((b.x, b.y, b.box_width, b.box_height), (60, 0, 1800, 1080));
        let b = Letterbox::new(SourceSpec::square(640, 360, PixelLayout::Bgrx), 1920, 1080);
        assert_eq!((b.box_width, b.box_height), (1920, 1080));
        let b = Letterbox::new(SourceSpec::square(1000, 600, PixelLayout::Bgrx), 1920, 1080);
        assert_eq!((b.x, b.y, b.box_width, b.box_height), (460, 240, 1000, 600));
        // Blue in BGRx order.
        let blue: Vec<u8> = (0..1000 * 600).flat_map(|_| [255, 0, 0, 0]).collect();
        let mut out = Vec::new();
        b.to_nv12(&blue, &mut out).unwrap();
        assert_eq!(out[1920 * 300 + 900], 32);
    }

    #[test]
    fn output_checks() {
        assert!(check_output(Path::new("clip.png")).is_err());
        assert!(check_output(Path::new("no-such-folder-xyz/clip.mp4")).is_err());
        assert_eq!(
            segment_sidecar(Path::new("a/b.mp4")),
            PathBuf::from("a/b.mp4.segment.json")
        );
    }
}
