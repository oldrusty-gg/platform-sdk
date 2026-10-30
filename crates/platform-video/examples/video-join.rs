//! Put recorded clips end to end in one MP4 (a tool):
//! `video-join <new.mp4> [--fps N] <clip.mp4>...`.
//! Every clip must have the output's picture size; each is re-encoded at
//! the output frame rate (default 50: the games' 25 Hz updates are two
//! pictures each), a clip at another rate by nearest earlier picture, and
//! its sound follows its pictures (padded with silence or cut to them).
#[cfg(windows)]
fn main() {
    if let Err(e) = run() {
        eprintln!("{e}");
        std::process::exit(1);
    }
}

#[cfg(not(windows))]
fn main() {
    eprintln!("video-join needs Windows (Media Foundation)");
    std::process::exit(1);
}

#[cfg(windows)]
fn run() -> Result<(), String> {
    use platform_video::{PixelLayout, Recorder, SegmentFormat, SourceSpec};
    use std::path::PathBuf;
    let mut args = std::env::args().skip(1);
    let out = PathBuf::from(args.next().ok_or("usage: video-join <new.mp4> [--fps N] <clip.mp4>...")?);
    let mut fps = 50u32;
    let mut clips = Vec::new();
    while let Some(a) = args.next() {
        if a == "--fps" {
            fps = args.next().and_then(|v| v.parse().ok()).ok_or("--fps needs a number")?;
        } else {
            clips.push(PathBuf::from(a));
        }
    }
    if clips.is_empty() {
        return Err("no clips".into());
    }
    let first = platform_video::probe(&clips[0])?;
    let format = SegmentFormat::new(first.width, first.height, fps)?;
    let (w, h) = (first.width as usize, first.height as usize);
    let source = SourceSpec::square(first.width, first.height, PixelLayout::Rgba);
    let mut rec = Recorder::start(&out, format, source, "video-join")?;
    let per = format.samples_per_frame() as usize;
    for clip in &clips {
        let p = platform_video::probe(clip)?;
        if (p.width, p.height) != (first.width, first.height) {
            return Err(format!("{} is {}x{}, not {w}x{h}", clip.display(), p.width, p.height));
        }
        let (num, den) = (u64::from(p.fps.0), u64::from(p.fps.1.max(1)));
        let sound = platform_video::decode_audio(clip)?;
        if sound.channels != 2 || sound.rate != 48_000 {
            return Err(format!("{}: sound is not 48 kHz stereo", clip.display()));
        }
        let mut k = 0u64; // output pictures from this clip
        let mut err = None;
        platform_video::decode_video(clip, |i, _, nv12, stride, rows, _| {
            if err.is_some() {
                return;
            }
            // Source picture i covers [i, i+1) / (num/den) s; emit every
            // output picture whose start falls in it.
            while u128::from(k) * u128::from(num) < u128::from(i + 1) * u128::from(den) * u128::from(fps) {
                let rgba = to_rgba(nv12, stride, rows, w, h);
                let at = k as usize * per * 2;
                let mut pcm = vec![0i16; per * 2];
                if at < sound.samples.len() {
                    let end = (at + per * 2).min(sound.samples.len());
                    pcm[..end - at].copy_from_slice(&sound.samples[at..end]);
                }
                if let Err(e) = rec.frame(rgba).and_then(|_| rec.audio(pcm)) {
                    err = Some(e);
                    return;
                }
                k += 1;
            }
        })?;
        if let Some(e) = err {
            return Err(e);
        }
        println!("{}: {} pictures at {fps} fps", clip.display(), k);
    }
    let s = rec.finish()?;
    println!("wrote {}: {} pictures, {:.3} s", out.display(), s.frames, s.frames as f64 / f64::from(fps));
    Ok(())
}

#[cfg(windows)]
fn to_rgba(nv12: &[u8], stride: usize, rows: usize, w: usize, h: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(w * h * 4);
    for y in 0..h {
        let c = rows * stride + (y / 2) * stride;
        for x in 0..w {
            let yy = (f32::from(nv12[y * stride + x]) - 16.0) * (255.0 / 219.0);
            let (u, v) = (f32::from(nv12[c + (x & !1)]) - 128.0, f32::from(nv12[c + (x & !1) + 1]) - 128.0);
            let ck = 255.0 / 224.0;
            let r = yy + ck * 1.5748 * v;
            let g = yy - ck * (0.1873 * u + 0.4681 * v);
            let b = yy + ck * 1.8556 * u;
            out.extend([r, g, b].map(|c| c.round().clamp(0.0, 255.0) as u8));
            out.push(255);
        }
    }
    out
}
