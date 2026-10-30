//! Look at a stretch of a recorded clip (a tool):
//! `video-frames <clip.mp4> <first> <last> <dir> [every]`.
//! Prints, for each picture in first..=last, its time and how much its
//! luma differs from the previous picture (0 = a repeated picture), and
//! writes every `every`-th picture (default 1) as a half-size PNG.
#[cfg(windows)]
fn main() {
    if let Err(e) = run() {
        eprintln!("{e}");
        std::process::exit(1);
    }
}

#[cfg(not(windows))]
fn main() {
    eprintln!("video-frames needs Windows (Media Foundation)");
    std::process::exit(1);
}

#[cfg(windows)]
fn run() -> Result<(), String> {
    use std::path::PathBuf;
    let a: Vec<String> = std::env::args().skip(1).collect();
    if a.len() < 4 {
        return Err("usage: video-frames <clip.mp4> <first> <last> <dir> [every]".into());
    }
    let clip = PathBuf::from(&a[0]);
    let first: u64 = a[1].parse().map_err(|_| "bad first")?;
    let last: u64 = a[2].parse().map_err(|_| "bad last")?;
    let dir = PathBuf::from(&a[3]);
    let every: u64 = a.get(4).and_then(|v| v.parse().ok()).unwrap_or(1).max(1);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let p = platform_video::probe(&clip)?;
    let (w, h) = (p.width as usize, p.height as usize);
    let mut prev: Option<Vec<u8>> = None;
    platform_video::decode_video(&clip, |i, ts, nv12, stride, rows, _| {
        if i < first.saturating_sub(1) {
            return;
        }
        if i > last {
            std::process::exit(0);
        }
        let luma: Vec<u8> = (0..h).flat_map(|y| nv12[y * stride..y * stride + w].iter().copied()).collect();
        if let Some(q) = &prev {
            let diff = luma
                .iter()
                .zip(q)
                .map(|(&x, &y)| u64::from(x.abs_diff(y)))
                .sum::<u64>() as f64
                / luma.len() as f64;
            println!("{i}\t{:.4}\t{diff:.3}", ts as f64 / 1e7);
        }
        if i >= first && (i - first) % every == 0 {
            let rgb = half_rgb(nv12, stride, rows, w, h);
            let out = dir.join(format!("f{i:06}.png"));
            if let Ok(file) = std::fs::File::create(&out) {
                let mut enc = png::Encoder::new(std::io::BufWriter::new(file), (w / 2) as u32, (h / 2) as u32);
                enc.set_color(png::ColorType::Rgb);
                let _ = enc.write_header().and_then(|mut wr| wr.write_image_data(&rgb));
            }
        }
        prev = Some(luma);
    })?;
    Ok(())
}

#[cfg(windows)]
fn half_rgb(nv12: &[u8], stride: usize, rows: usize, w: usize, h: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(w * h * 3 / 4);
    for y in (0..h).step_by(2) {
        for x in (0..w).step_by(2) {
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
