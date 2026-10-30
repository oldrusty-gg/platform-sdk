//! A small MP4 (ISO BMFF) box reader for checking recordings: the file
//! brand, the top-level boxes, and per track its handler, codec, time scale,
//! duration and sample count. `mdat` is skipped, never read.
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Track {
    /// `vide` or `soun`.
    pub handler: String,
    /// First sample entry: `avc1`, `mp4a`, ...
    pub codec: String,
    pub timescale: u32,
    pub duration: u64,
    pub samples: u64,
    /// `tkhd` size (video tracks).
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Mp4Info {
    pub major_brand: String,
    pub compatible: Vec<String>,
    /// Top-level box types in file order.
    pub boxes: Vec<String>,
    pub tracks: Vec<Track>,
}

impl Mp4Info {
    pub fn track(&self, handler: &str) -> Option<&Track> {
        self.tracks.iter().find(|t| t.handler == handler)
    }
}

pub fn inspect(path: &Path) -> Result<Mp4Info, String> {
    let mut file = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    inspect_reader(&mut file)
}

pub fn inspect_reader(r: &mut (impl Read + Seek)) -> Result<Mp4Info, String> {
    let end = r.seek(SeekFrom::End(0)).map_err(|e| e.to_string())?;
    r.seek(SeekFrom::Start(0)).map_err(|e| e.to_string())?;
    let mut info = Mp4Info::default();
    let mut at = 0u64;
    while at + 8 <= end {
        let mut head = [0u8; 8];
        r.read_exact(&mut head).map_err(|e| e.to_string())?;
        let mut size = u64::from(u32::from_be_bytes([head[0], head[1], head[2], head[3]]));
        let kind = fourcc(&head[4..8]);
        let mut header = 8;
        if size == 1 {
            let mut large = [0u8; 8];
            r.read_exact(&mut large).map_err(|e| e.to_string())?;
            size = u64::from_be_bytes(large);
            header = 16;
        } else if size == 0 {
            size = end - at;
        }
        if size < header || at + size > end {
            return Err(format!("box {kind} at {at} has a bad size {size}"));
        }
        info.boxes.push(kind.clone());
        let body = size - header;
        match kind.as_str() {
            "ftyp" | "moov" if body <= 64 << 20 => {
                let mut bytes = vec![0u8; body as usize];
                r.read_exact(&mut bytes).map_err(|e| e.to_string())?;
                if kind == "ftyp" {
                    if bytes.len() < 8 {
                        return Err("ftyp is too short".into());
                    }
                    info.major_brand = fourcc(&bytes[0..4]);
                    info.compatible = bytes[8..].chunks_exact(4).map(fourcc).collect();
                } else {
                    for (k, trak) in children(&bytes)? {
                        if k == "trak" {
                            info.tracks.push(track(trak)?);
                        }
                    }
                }
            }
            _ => {
                r.seek(SeekFrom::Current(body as i64))
                    .map_err(|e| e.to_string())?;
            }
        }
        at += size;
    }
    Ok(info)
}

fn fourcc(b: &[u8]) -> String {
    b.iter()
        .map(|&c| {
            if c.is_ascii_graphic() || c == b' ' {
                c as char
            } else {
                '?'
            }
        })
        .collect()
}

fn be32(b: &[u8], at: usize) -> Result<u32, String> {
    b.get(at..at + 4)
        .map(|s| u32::from_be_bytes([s[0], s[1], s[2], s[3]]))
        .ok_or_else(|| "box too short".into())
}

fn be64(b: &[u8], at: usize) -> Result<u64, String> {
    Ok(u64::from(be32(b, at)?) << 32 | u64::from(be32(b, at + 4)?))
}

/// The boxes inside a container's body.
fn children(body: &[u8]) -> Result<Vec<(String, &[u8])>, String> {
    let mut out = Vec::new();
    let mut at = 0usize;
    while at + 8 <= body.len() {
        let mut size = be32(body, at)? as usize;
        let kind = fourcc(&body[at + 4..at + 8]);
        let mut header = 8;
        if size == 1 {
            size = be64(body, at + 8)? as usize;
            header = 16;
        } else if size == 0 {
            size = body.len() - at;
        }
        if size < header || at + size > body.len() {
            return Err(format!("box {kind} has a bad size {size}"));
        }
        out.push((kind, &body[at + header..at + size]));
        at += size;
    }
    Ok(out)
}

fn find<'a>(body: &'a [u8], path: &[&str]) -> Result<Option<&'a [u8]>, String> {
    let Some((first, rest)) = path.split_first() else {
        return Ok(Some(body));
    };
    for (k, b) in children(body)? {
        if k == *first {
            return find(b, rest);
        }
    }
    Ok(None)
}

fn track(trak: &[u8]) -> Result<Track, String> {
    let mut t = Track::default();
    if let Some(tkhd) = find(trak, &["tkhd"])? {
        if tkhd.len() >= 8 {
            t.width = be32(tkhd, tkhd.len() - 8)? >> 16;
            t.height = be32(tkhd, tkhd.len() - 4)? >> 16;
        }
    }
    let mdia = find(trak, &["mdia"])?.ok_or("trak without mdia")?;
    if let Some(hdlr) = find(mdia, &["hdlr"])? {
        t.handler = fourcc(hdlr.get(8..12).ok_or("hdlr too short")?);
    }
    if let Some(mdhd) = find(mdia, &["mdhd"])? {
        if mdhd.first() == Some(&1) {
            t.timescale = be32(mdhd, 20)?;
            t.duration = be64(mdhd, 24)?;
        } else {
            t.timescale = be32(mdhd, 12)?;
            t.duration = u64::from(be32(mdhd, 16)?);
        }
    }
    if let Some(stbl) = find(mdia, &["minf", "stbl"])? {
        if let Some(stsd) = find(stbl, &["stsd"])? {
            if stsd.len() >= 16 {
                t.codec = fourcc(&stsd[12..16]);
            }
        }
        if let Some(stsz) = find(stbl, &["stsz"])? {
            t.samples = u64::from(be32(stsz, 8)?);
        } else if let Some(stz2) = find(stbl, &["stz2"])? {
            t.samples = u64::from(be32(stz2, 8)?);
        }
    }
    Ok(t)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn boxed(kind: &str, body: &[u8]) -> Vec<u8> {
        let mut v = ((body.len() + 8) as u32).to_be_bytes().to_vec();
        v.extend_from_slice(kind.as_bytes());
        v.extend_from_slice(body);
        v
    }

    #[test]
    fn reads_a_synthetic_file() {
        let mut ftyp = b"mp42".to_vec();
        ftyp.extend_from_slice(&0u32.to_be_bytes());
        ftyp.extend_from_slice(b"isommp42");
        let hdlr = boxed(
            "hdlr",
            &[0, 0, 0, 0, 0, 0, 0, 0, b'v', b'i', b'd', b'e', 0, 0, 0, 0],
        );
        let mut mdhd_body = vec![0u8; 12];
        mdhd_body.extend_from_slice(&60_000u32.to_be_bytes());
        mdhd_body.extend_from_slice(&600_000u32.to_be_bytes());
        mdhd_body.extend_from_slice(&[0; 4]);
        let mdhd = boxed("mdhd", &mdhd_body);
        let mut stsd_body = vec![0, 0, 0, 0, 0, 0, 0, 1];
        stsd_body.extend_from_slice(&boxed("avc1", &[0; 8]));
        let mut stsz_body = vec![0u8; 8];
        stsz_body.extend_from_slice(&600u32.to_be_bytes());
        let stbl = boxed(
            "stbl",
            &[boxed("stsd", &stsd_body), boxed("stsz", &stsz_body)].concat(),
        );
        let minf = boxed("minf", &stbl);
        let mdia = boxed("mdia", &[hdlr, mdhd, minf].concat());
        let mut tkhd_body = vec![0u8; 76];
        tkhd_body.extend_from_slice(&(1920u32 << 16).to_be_bytes());
        tkhd_body.extend_from_slice(&(1080u32 << 16).to_be_bytes());
        let trak = boxed("trak", &[boxed("tkhd", &tkhd_body), mdia].concat());
        let file = [
            boxed("ftyp", &ftyp),
            boxed("mdat", &[1, 2, 3]),
            boxed("moov", &trak),
        ]
        .concat();
        let info = inspect_reader(&mut std::io::Cursor::new(file)).unwrap();
        assert_eq!(info.major_brand, "mp42");
        assert_eq!(info.boxes, ["ftyp", "mdat", "moov"]);
        let v = info.track("vide").unwrap();
        assert_eq!(
            (v.codec.as_str(), v.samples, v.timescale, v.duration),
            ("avc1", 600, 60_000, 600_000)
        );
        assert_eq!((v.width, v.height), (1920, 1080));
    }

    #[test]
    fn rejects_truncated_boxes() {
        let mut file = boxed("ftyp", b"mp42\0\0\0\0");
        file.extend_from_slice(&100u32.to_be_bytes());
        file.extend_from_slice(b"moov");
        assert!(inspect_reader(&mut std::io::Cursor::new(file)).is_err());
    }
}
