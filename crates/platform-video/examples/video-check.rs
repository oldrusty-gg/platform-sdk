//! Check recorded clips (a tool): `video-check <clip.mp4>... [--png <frame> <dir>]`.
//! Prints each clip's MP4 tracks, its read-back size, frame count and sound
//! length, checks its `.segment.json`, and that every clip shares one
//! segment format (so an editor can put them end to end). `--png N DIR`
//! also writes picture N of each clip as a PNG (decoded through Media
//! Foundation, BT.709 limited range back to RGB).
#[cfg(windows)]
fn main() {
    if let Err(e) = run() {
        eprintln!("{e}");
        std::process::exit(1);
    }
}

#[cfg(not(windows))]
fn main() {
    eprintln!("video-check needs Windows (Media Foundation)");
    std::process::exit(1);
}

#[cfg(windows)]
fn run() -> Result<(), String> {
    use std::path::PathBuf;
    let mut clips = Vec::new();
    let mut png: Option<(u64, PathBuf)> = None;
    let mut args = std::env::args_os().skip(1);
    while let Some(a) = args.next() {
        if a == "--png" {
            let n = args
                .next()
                .and_then(|v| v.to_str()?.parse().ok())
                .ok_or("--png needs a frame number")?;
            png = Some((n, PathBuf::from(args.next().ok_or("--png needs a folder")?)));
        } else {
            clips.push(PathBuf::from(a));
        }
    }
    if clips.is_empty() {
        return Err("usage: video-check <clip.mp4>... [--png <frame> <dir>]".into());
    }
    let mut formats = Vec::new();
    let mut ok = true;
    for clip in &clips {
        let info = platform_video::mp4::inspect(clip)?;
        let p = platform_video::probe(clip)?;
        let side = std::fs::read(platform_video::segment_sidecar(clip))
            .ok()
            .and_then(|b| serde_json::from_slice::<platform_video::SegmentInfo>(&b).ok());
        let seconds = (p.video_end - p.video_start) as f64 / 1e7;
        let sound = platform_video::decode_audio(clip)?;
        let peak = sound
            .samples
            .iter()
            .map(|s| s.unsigned_abs())
            .max()
            .unwrap_or(0);
        let rms = (sound
            .samples
            .iter()
            .map(|&s| f64::from(s) * f64::from(s))
            .sum::<f64>()
            / sound.samples.len().max(1) as f64)
            .sqrt();
        println!(
            "{}\n  boxes {:?}; tracks {:?}\n  read back: {}x{} at {}/{} fps, {} pictures, {:.3} s; sound {} Hz x{}, {:.3} s, peak {}, rms {:.0}",
            clip.display(),
            info.boxes,
            info.tracks.iter().map(|t| format!("{} {} ({} samples)", t.handler, t.codec, t.samples)).collect::<Vec<_>>(),
            p.width,
            p.height,
            p.fps.0,
            p.fps.1,
            p.video_frames,
            seconds,
            p.audio_rate,
            p.audio_channels,
            p.audio_frames as f64 / f64::from(p.audio_rate.max(1)),
            peak,
            rms,
        );
        match &side {
            Some(s) => {
                let same = s.frames == p.video_frames
                    && s.format.width == p.width
                    && s.format.height == p.height;
                ok &= same;
                println!(
                    "  segment: {} from {}, {} frames{}",
                    s.schema,
                    s.source,
                    s.frames,
                    if same {
                        ""
                    } else {
                        " (DOES NOT MATCH the file)"
                    }
                );
                formats.push(s.format);
            }
            None => {
                ok = false;
                println!(
                    "  segment: no {} sidecar",
                    platform_video::segment_sidecar(clip).display()
                );
            }
        }
        if let Some((frame, dir)) = &png {
            std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
            let name = format!(
                "{}-frame{frame}.png",
                clip.file_stem().and_then(|s| s.to_str()).unwrap_or("clip")
            );
            let out = dir.join(name);
            let (w, h) = (p.width as usize, p.height as usize);
            let mut rgb = None;
            platform_video::decode_video(clip, |i, _, nv12, stride, rows, _| {
                if i == *frame {
                    rgb = Some(to_rgb(nv12, stride, rows, w, h));
                }
            })?;
            let rgb = rgb.ok_or_else(|| format!("{} has no picture {frame}", clip.display()))?;
            let file = std::fs::File::create(&out).map_err(|e| e.to_string())?;
            let mut enc = png::Encoder::new(std::io::BufWriter::new(file), w as u32, h as u32);
            enc.set_color(png::ColorType::Rgb);
            enc.write_header()
                .and_then(|mut wr| wr.write_image_data(&rgb))
                .map_err(|e| e.to_string())?;
            println!("  wrote {}", out.display());
        }
    }
    if formats.windows(2).any(|w| w[0] != w[1]) {
        ok = false;
        println!("The clips do not share one segment format: {formats:?}");
    } else if clips.len() > 1 {
        println!("All {} clips share one segment format.", clips.len());
    }
    if ok {
        Ok(())
    } else {
        Err("check failed".into())
    }
}

#[cfg(windows)]
fn to_rgb(nv12: &[u8], stride: usize, rows: usize, w: usize, h: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(w * h * 3);
    for y in 0..h {
        for x in 0..w {
            let yy = f32::from(nv12[y * stride + x]) - 16.0;
            let c = rows * stride + (y / 2) * stride + (x & !1);
            let (u, v) = (f32::from(nv12[c]) - 128.0, f32::from(nv12[c + 1]) - 128.0);
            let k = 255.0 / 219.0;
            let ck = 255.0 / 224.0;
            let r = k * yy + ck * 1.5748 * v;
            let g = k * yy - ck * (0.1873 * u + 0.4681 * v);
            let b = k * yy + ck * 1.8556 * u;
            out.extend([r, g, b].map(|c| c.round().clamp(0.0, 255.0) as u8));
        }
    }
    out
}
