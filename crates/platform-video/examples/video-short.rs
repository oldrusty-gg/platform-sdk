//! Cut a vertical Short from a recorded clip (a tool):
//! `video-short <clip.mp4> <new.mp4> <from_s> <to_s> <overlay.png> [picture_y]`.
//! The clip's 4:3 picture (the middle of its 16:9 frame) is scaled to the
//! overlay's width and drawn over the overlay (a full-size RGBA PNG, e.g.
//! 1080x1920 with titles above and below) at `picture_y` (default: centred).
//! The sound is the clip's own for the same span. Same frame rate as the clip.
#[cfg(windows)]
fn main() {
    if let Err(e) = run() {
        eprintln!("{e}");
        std::process::exit(1);
    }
}

#[cfg(not(windows))]
fn main() {
    eprintln!("video-short needs Windows (Media Foundation)");
    std::process::exit(1);
}

#[cfg(windows)]
fn run() -> Result<(), String> {
    use platform_video::{PixelLayout, Recorder, SegmentFormat, SourceSpec};
    use std::path::PathBuf;
    let a: Vec<String> = std::env::args().skip(1).collect();
    if a.len() < 5 {
        return Err("usage: video-short <clip.mp4> <new.mp4> <from_s> <to_s> <overlay.png> [picture_y]".into());
    }
    let (clip, out) = (PathBuf::from(&a[0]), PathBuf::from(&a[1]));
    let from: f64 = a[2].parse().map_err(|_| "bad from")?;
    let to: f64 = a[3].parse().map_err(|_| "bad to")?;
    let (ow, oh, overlay) = read_png(&PathBuf::from(&a[4]))?;
    let p = platform_video::probe(&clip)?;
    let fps = p.fps.0 / p.fps.1.max(1);
    let (sw, sh) = (p.width as usize, p.height as usize);
    // The 4:3 picture inside the 16:9 frame.
    let (bw, bh) = (sh * 4 / 3, sh);
    let bx = (sw - bw) / 2;
    let (pw, ph) = (ow, ow * bh / bw);
    let py: usize = a.get(5).and_then(|v| v.parse().ok()).unwrap_or((oh - ph) / 2);
    if py + ph > oh {
        return Err("the picture does not fit the overlay".into());
    }
    let format = SegmentFormat::new(ow as u32, oh as u32, fps)?;
    let rec = Recorder::start(&out, format, SourceSpec::square(ow as u32, oh as u32, PixelLayout::Rgba), "video-short")?;
    let sound = platform_video::decode_audio(&clip)?;
    let per = format.samples_per_frame() as usize;
    let (first, last) = ((from * f64::from(fps)).round() as u64, (to * f64::from(fps)).round() as u64);
    let xmap: Vec<usize> = (0..pw).map(|x| bx + (x * 2 + 1) * bw / (pw * 2)).collect();
    let ymap: Vec<usize> = (0..ph).map(|y| (y * 2 + 1) * bh / (ph * 2)).collect();
    let mut err = None;
    let mut n = 0u64;
    let finish = |rec: Recorder, n: u64| -> Result<(), String> {
        let s = rec.finish()?;
        println!("wrote {}: {} pictures ({n} cut), {:.2} s", out.display(), s.frames, s.frames as f64 / f64::from(fps));
        Ok(())
    };
    let mut rec = Some(rec);
    platform_video::decode_video(&clip, |i, _, nv12, stride, rows, _| {
        if err.is_some() || i < first || i >= last {
            return;
        }
        let Some(r) = rec.as_mut() else { return };
        let mut img = overlay.clone();
        for (y, &sy) in ymap.iter().enumerate() {
            let c = rows * stride + (sy / 2) * stride;
            for (x, &sx) in xmap.iter().enumerate() {
                let yy = (f32::from(nv12[sy * stride + sx]) - 16.0) * (255.0 / 219.0);
                let (u, v) = (f32::from(nv12[c + (sx & !1)]) - 128.0, f32::from(nv12[c + (sx & !1) + 1]) - 128.0);
                let ck = 255.0 / 224.0;
                let rgb = [yy + ck * 1.5748 * v, yy - ck * (0.1873 * u + 0.4681 * v), yy + ck * 1.8556 * u];
                let o = ((py + y) * ow + x) * 4;
                for k in 0..3 {
                    img[o + k] = rgb[k].round().clamp(0.0, 255.0) as u8;
                }
                img[o + 3] = 255;
            }
        }
        let at = i as usize * per * 2;
        let mut pcm = vec![0i16; per * 2];
        if at < sound.samples.len() {
            let end = (at + per * 2).min(sound.samples.len());
            pcm[..end - at].copy_from_slice(&sound.samples[at..end]);
        }
        if let Err(e) = r.frame(img).and_then(|_| r.audio(pcm)) {
            err = Some(e);
            return;
        }
        n += 1;
        if i + 1 == last {
            let r = rec.take().unwrap();
            if let Err(e) = finish(r, n) {
                eprintln!("{e}");
                std::process::exit(1);
            }
            std::process::exit(0);
        }
    })?;
    if let Some(e) = err {
        return Err(e);
    }
    match rec {
        Some(r) => finish(r, n),
        None => Ok(()),
    }
}

/// An RGBA (or RGB) PNG as RGBA bytes.
#[cfg(windows)]
fn read_png(path: &std::path::Path) -> Result<(usize, usize, Vec<u8>), String> {
    let file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut dec = png::Decoder::new(std::io::BufReader::new(file)).read_info().map_err(|e| e.to_string())?;
    let mut buf = vec![0; dec.output_buffer_size().ok_or("png too large")?];
    let info = dec.next_frame(&mut buf).map_err(|e| e.to_string())?;
    let (w, h) = (info.width as usize, info.height as usize);
    let px = &buf[..info.buffer_size()];
    let rgba = match info.color_type {
        png::ColorType::Rgba => px.to_vec(),
        png::ColorType::Rgb => px.chunks(3).flat_map(|c| [c[0], c[1], c[2], 255]).collect(),
        t => return Err(format!("{}: PNG colour type {t:?} not supported", path.display())),
    };
    Ok((w, h, rgba))
}
