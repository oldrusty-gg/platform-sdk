//! Media Foundation: the Sink Writer that writes the clip, and the Source
//! Reader that reads one back (tests, checks).
use crate::{
    check_output, segment_sidecar, Letterbox, SegmentFormat, SegmentInfo, SourceSpec,
    SEGMENT_SCHEMA,
};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{sync_channel, Receiver, SyncSender};
use std::thread::JoinHandle;
use windows::core::HSTRING;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED};

/// Frames queued for the encoder before `frame` waits (bounded memory).
const QUEUE: usize = 6;

fn hr(what: &str) -> impl Fn(windows_core::Error) -> String + '_ {
    move |e| format!("Media Foundation: {what}: {e}")
}

fn pack(hi: u32, lo: u32) -> u64 {
    u64::from(hi) << 32 | u64::from(lo)
}

/// COM and Media Foundation for one thread.
struct Session {
    com: bool,
}

impl Session {
    fn start() -> Result<Self, String> {
        // SAFETY: plain initialisation calls, balanced in Drop.
        unsafe {
            let com = CoInitializeEx(None, COINIT_MULTITHREADED).is_ok();
            if let Err(e) = MFStartup(MF_VERSION, MFSTARTUP_FULL) {
                if com {
                    CoUninitialize();
                }
                return Err(format!("Media Foundation is not available: {e}"));
            }
            Ok(Self { com })
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // SAFETY: balances `start` on the same thread.
        unsafe {
            let _ = MFShutdown();
            if self.com {
                CoUninitialize();
            }
        }
    }
}

enum Msg {
    Frame(Vec<u8>),
    Audio(Vec<i16>),
}

/// What a finished recording holds.
#[derive(Clone, Debug, PartialEq)]
pub struct Summary {
    pub frames: u64,
    pub audio_frames: u64,
    pub hardware: bool,
}

/// An MP4 being written by an encoder thread. Hand it one frame and
/// `samples_per_frame` stereo sound frames per picture.
pub struct Recorder {
    path: PathBuf,
    format: SegmentFormat,
    letterbox: Letterbox,
    label: String,
    tx: Option<SyncSender<Msg>>,
    thread: Option<JoinHandle<Result<Summary, String>>>,
    frames: u64,
    audio_frames: u64,
}

impl Recorder {
    /// Open `path` (a new .mp4) and start the encoder. `label` names the
    /// recording program in the sidecar.
    pub fn start(
        path: &Path,
        format: SegmentFormat,
        source: SourceSpec,
        label: &str,
    ) -> Result<Self, String> {
        format.validate()?;
        check_output(path)?;
        let letterbox = Letterbox::new(source, format.width, format.height);
        let (tx, rx) = sync_channel(QUEUE);
        let (ready_tx, ready_rx) = sync_channel(1);
        let (p, lb) = (path.to_path_buf(), letterbox.clone());
        let thread = std::thread::Builder::new()
            .name("video record".into())
            .spawn(move || encode(&p, format, lb, rx, ready_tx))
            .map_err(|e| e.to_string())?;
        match ready_rx.recv() {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                let _ = thread.join();
                return Err(e);
            }
            Err(_) => {
                return Err(thread
                    .join()
                    .ok()
                    .and_then(|r| r.err())
                    .unwrap_or_else(|| "video encoder stopped".into()));
            }
        }
        Ok(Self {
            path: path.to_path_buf(),
            format,
            letterbox,
            label: label.to_owned(),
            tx: Some(tx),
            thread: Some(thread),
            frames: 0,
            audio_frames: 0,
        })
    }

    pub fn format(&self) -> SegmentFormat {
        self.format
    }

    /// Pictures handed over so far.
    pub fn frames(&self) -> u64 {
        self.frames
    }

    /// Sound frames handed over so far.
    pub fn audio_frames(&self) -> u64 {
        self.audio_frames
    }

    fn send(&mut self, msg: Msg) -> Result<(), String> {
        let sent = self
            .tx
            .as_ref()
            .map(|tx| tx.send(msg).is_ok())
            .unwrap_or(false);
        if sent {
            return Ok(());
        }
        self.tx = None;
        Err(match self.thread.take().map(JoinHandle::join) {
            Some(Ok(Err(e))) => e,
            _ => "video encoder stopped".into(),
        })
    }

    /// One picture (the source's pixels, 4 bytes each).
    pub fn frame(&mut self, pixels: Vec<u8>) -> Result<(), String> {
        let s = self.letterbox.source;
        if pixels.len() != s.width as usize * s.height as usize * 4 {
            return Err(format!(
                "frame has {} bytes; {}x{} needs {}",
                pixels.len(),
                s.width,
                s.height,
                s.width * s.height * 4
            ));
        }
        self.send(Msg::Frame(pixels))?;
        self.frames += 1;
        Ok(())
    }

    /// Interleaved stereo sound at 48 kHz.
    pub fn audio(&mut self, pcm: Vec<i16>) -> Result<(), String> {
        if pcm.is_empty() {
            return Ok(());
        }
        if !pcm.len().is_multiple_of(2) {
            return Err("stereo sound needs an even number of samples".into());
        }
        let n = pcm.len() as u64 / 2;
        self.send(Msg::Audio(pcm))?;
        self.audio_frames += n;
        Ok(())
    }

    /// Pad the sound to the pictures, finish the file and write its sidecar.
    pub fn finish(mut self) -> Result<Summary, String> {
        let wanted = self.frames * u64::from(self.format.samples_per_frame());
        if self.audio_frames < wanted {
            let pad = (wanted - self.audio_frames) as usize;
            self.audio(vec![0; pad * 2])?;
        }
        self.tx = None;
        let summary = match self.thread.take().map(JoinHandle::join) {
            Some(Ok(result)) => result?,
            _ => return Err("video encoder panicked".into()),
        };
        let info = SegmentInfo {
            schema: SEGMENT_SCHEMA.into(),
            format: self.format,
            source: self.label.clone(),
            source_size: [self.letterbox.source.width, self.letterbox.source.height],
            picture_box: [
                self.letterbox.x,
                self.letterbox.y,
                self.letterbox.box_width,
                self.letterbox.box_height,
            ],
            frames: summary.frames,
            audio_frames: summary.audio_frames,
            duration_seconds: summary.frames as f64 / f64::from(self.format.fps),
        };
        let json = serde_json::to_string_pretty(&info).map_err(|e| e.to_string())?;
        std::fs::write(segment_sidecar(&self.path), json + "\n").map_err(|e| e.to_string())?;
        Ok(summary)
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        // Unfinished (an error elsewhere): stop the thread; it finalizes
        // what it has.
        self.tx = None;
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

struct Writer {
    sink: IMFSinkWriter,
    video: u32,
    audio: u32,
}

fn media_type(
    major: &windows_core::GUID,
    sub: &windows_core::GUID,
) -> Result<IMFMediaType, String> {
    // SAFETY: COM calls on a freshly created object.
    unsafe {
        let t = MFCreateMediaType().map_err(hr("media type"))?;
        t.SetGUID(&MF_MT_MAJOR_TYPE, major)
            .map_err(hr("major type"))?;
        t.SetGUID(&MF_MT_SUBTYPE, sub).map_err(hr("subtype"))?;
        Ok(t)
    }
}

fn video_type(t: &IMFMediaType, f: &SegmentFormat) -> Result<(), String> {
    // SAFETY: attribute setters on a live media type.
    unsafe {
        t.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)
            .map_err(hr("interlace"))?;
        t.SetUINT64(&MF_MT_FRAME_SIZE, pack(f.width, f.height))
            .map_err(hr("frame size"))?;
        t.SetUINT64(&MF_MT_FRAME_RATE, pack(f.fps, 1))
            .map_err(hr("frame rate"))?;
        t.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pack(1, 1))
            .map_err(hr("pixel aspect"))?;
        t.SetUINT32(&MF_MT_YUV_MATRIX, MFVideoTransferMatrix_BT709.0 as u32)
            .map_err(hr("matrix"))?;
        t.SetUINT32(&MF_MT_VIDEO_NOMINAL_RANGE, MFNominalRange_16_235.0 as u32)
            .map_err(hr("range"))?;
        t.SetUINT32(&MF_MT_VIDEO_PRIMARIES, MFVideoPrimaries_BT709.0 as u32)
            .map_err(hr("primaries"))?;
        t.SetUINT32(&MF_MT_TRANSFER_FUNCTION, MFVideoTransFunc_709.0 as u32)
            .map_err(hr("transfer"))?;
    }
    Ok(())
}

fn audio_type(t: &IMFMediaType, f: &SegmentFormat, avg_bytes: u32) -> Result<(), String> {
    // SAFETY: attribute setters on a live media type.
    unsafe {
        t.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)
            .map_err(hr("bits"))?;
        t.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, f.sample_rate)
            .map_err(hr("rate"))?;
        t.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, u32::from(f.channels))
            .map_err(hr("channels"))?;
        t.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, avg_bytes)
            .map_err(hr("byte rate"))?;
    }
    Ok(())
}

fn open_writer(path: &Path, f: &SegmentFormat, hardware: bool) -> Result<Writer, String> {
    // SAFETY: Media Foundation calls on this thread's session.
    unsafe {
        let mut attrs: Option<IMFAttributes> = None;
        MFCreateAttributes(&mut attrs, 4).map_err(hr("attributes"))?;
        let attrs = attrs.ok_or("Media Foundation: no attributes")?;
        attrs
            .SetUINT32(
                &MF_READWRITE_ENABLE_HARDWARE_TRANSFORMS,
                u32::from(hardware),
            )
            .map_err(hr("hardware"))?;
        attrs
            .SetGUID(&MF_TRANSCODE_CONTAINERTYPE, &MFTranscodeContainerType_MPEG4)
            .map_err(hr("container"))?;
        let sink = MFCreateSinkWriterFromURL(
            &HSTRING::from(path.as_os_str()),
            None::<&IMFByteStream>,
            &attrs,
        )
        .map_err(hr("create the MP4 writer"))?;
        let out = media_type(&MFMediaType_Video, &MFVideoFormat_H264)?;
        video_type(&out, f)?;
        out.SetUINT32(&MF_MT_AVG_BITRATE, f.video_bitrate)
            .map_err(hr("bitrate"))?;
        out.SetUINT32(&MF_MT_MPEG2_PROFILE, eAVEncH264VProfile_High.0 as u32)
            .map_err(hr("profile"))?;
        let video = sink.AddStream(&out).map_err(hr("add the H.264 stream"))?;
        let input = media_type(&MFMediaType_Video, &MFVideoFormat_NV12)?;
        video_type(&input, f)?;
        input
            .SetUINT32(&MF_MT_DEFAULT_STRIDE, f.width)
            .map_err(hr("stride"))?;
        input
            .SetUINT32(&MF_MT_ALL_SAMPLES_INDEPENDENT, 1)
            .map_err(hr("independent"))?;
        // No B-frames: the software encoder otherwise shifts every picture
        // one frame late (a composition offset the MP4 has no edit for).
        let mut params: Option<IMFAttributes> = None;
        MFCreateAttributes(&mut params, 1).map_err(hr("encoder settings"))?;
        let params = params.ok_or("Media Foundation: no encoder settings")?;
        params
            .SetUINT32(&CODECAPI_AVEncMPVDefaultBPictureCount, 0)
            .map_err(hr("B-frames"))?;
        sink.SetInputMediaType(video, &input, &params)
            .map_err(hr("H.264 encoder input (NV12)"))?;
        let out = media_type(&MFMediaType_Audio, &MFAudioFormat_AAC)?;
        audio_type(&out, f, f.audio_bitrate / 8)?;
        out.SetUINT32(&MF_MT_AAC_PAYLOAD_TYPE, 0)
            .map_err(hr("AAC payload"))?;
        let audio = sink.AddStream(&out).map_err(hr("add the AAC stream"))?;
        let input = media_type(&MFMediaType_Audio, &MFAudioFormat_PCM)?;
        audio_type(&input, f, f.sample_rate * 4)?;
        input
            .SetUINT32(&MF_MT_AUDIO_BLOCK_ALIGNMENT, 4)
            .map_err(hr("block align"))?;
        input
            .SetUINT32(&MF_MT_ALL_SAMPLES_INDEPENDENT, 1)
            .map_err(hr("independent"))?;
        sink.SetInputMediaType(audio, &input, None::<&IMFAttributes>)
            .map_err(hr("AAC encoder input (PCM)"))?;
        sink.BeginWriting().map_err(hr("begin writing"))?;
        Ok(Writer { sink, video, audio })
    }
}

fn sample(bytes: &[u8], time: i64, duration: i64) -> Result<IMFSample, String> {
    // SAFETY: the buffer is locked for exactly `bytes.len()` bytes.
    unsafe {
        let len = bytes.len() as u32;
        let buffer = MFCreateMemoryBuffer(len).map_err(hr("buffer"))?;
        let mut data = std::ptr::null_mut();
        buffer.Lock(&mut data, None, None).map_err(hr("lock"))?;
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), data, bytes.len());
        buffer.Unlock().map_err(hr("unlock"))?;
        buffer.SetCurrentLength(len).map_err(hr("length"))?;
        let s = MFCreateSample().map_err(hr("sample"))?;
        s.AddBuffer(&buffer).map_err(hr("add buffer"))?;
        s.SetSampleTime(time).map_err(hr("time"))?;
        s.SetSampleDuration(duration).map_err(hr("duration"))?;
        Ok(s)
    }
}

fn encode(
    path: &Path,
    f: SegmentFormat,
    letterbox: Letterbox,
    rx: Receiver<Msg>,
    ready: SyncSender<Result<(), String>>,
) -> Result<Summary, String> {
    let _session = match Session::start() {
        Ok(s) => s,
        Err(e) => {
            let _ = ready.send(Err(e.clone()));
            return Err(e);
        }
    };
    // ROSES_RECORD_ENCODER=software skips a hardware H.264 encoder.
    let software_only =
        std::env::var("ROSES_RECORD_ENCODER").is_ok_and(|v| v.eq_ignore_ascii_case("software"));
    let opened = if software_only {
        open_writer(path, &f, false).map(|w| (w, false))
    } else {
        open_writer(path, &f, true)
            .map(|w| (w, true))
            .or_else(|first| {
                let _ = std::fs::remove_file(path);
                open_writer(path, &f, false)
                    .map(|w| (w, false))
                    .map_err(|e| format!("{first}; software: {e}"))
            })
    };
    let (writer, hardware) = match opened {
        Ok(w) => w,
        Err(e) => {
            let _ = std::fs::remove_file(path);
            let _ = ready.send(Err(e.clone()));
            return Err(e);
        }
    };
    let _ = ready.send(Ok(()));
    let mut frames = 0u64;
    let mut audio_frames = 0u64;
    let mut nv12 = Vec::with_capacity(letterbox.nv12_len());
    let mut result = Ok(());
    for msg in rx {
        let step = match msg {
            Msg::Frame(pixels) => letterbox.to_nv12(&pixels, &mut nv12).and_then(|()| {
                let t = f.frame_time(frames);
                let s = sample(&nv12, t, f.frame_time(frames + 1) - t)?;
                // SAFETY: a live writer and sample.
                unsafe { writer.sink.WriteSample(writer.video, &s) }
                    .map_err(hr("write a video frame"))?;
                frames += 1;
                Ok(())
            }),
            Msg::Audio(pcm) => {
                let n = pcm.len() as u64 / 2;
                let t = f.sample_time(audio_frames);
                let bytes: Vec<u8> = pcm.iter().flat_map(|v| v.to_le_bytes()).collect();
                sample(&bytes, t, f.sample_time(audio_frames + n) - t).and_then(|s| {
                    // SAFETY: a live writer and sample.
                    unsafe { writer.sink.WriteSample(writer.audio, &s) }
                        .map_err(hr("write sound"))?;
                    audio_frames += n;
                    Ok(())
                })
            }
        };
        if let Err(e) = step {
            result = Err(e);
            break;
        }
    }
    // SAFETY: a live writer; Finalize writes the moov box.
    let finalized = unsafe { writer.sink.Finalize() }.map_err(hr("finish the MP4"));
    result?;
    finalized?;
    Ok(Summary {
        frames,
        audio_frames,
        hardware,
    })
}

/// What a recording holds, as Media Foundation reads it back.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Probe {
    pub width: u32,
    pub height: u32,
    pub fps: (u32, u32),
    pub video_frames: u64,
    /// Start of the first picture and end of the last (100 ns).
    pub video_start: i64,
    pub video_end: i64,
    pub audio_rate: u32,
    pub audio_channels: u32,
    /// Sound frames decoded.
    pub audio_frames: u64,
    pub audio_start: i64,
}

const VIDEO: u32 = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;
const AUDIO: u32 = MF_SOURCE_READER_FIRST_AUDIO_STREAM.0 as u32;

fn reader(path: &Path) -> Result<IMFSourceReader, String> {
    // SAFETY: Media Foundation calls with a live session.
    unsafe {
        let r =
            MFCreateSourceReaderFromURL(&HSTRING::from(path.as_os_str()), None::<&IMFAttributes>)
                .map_err(hr("open the MP4"))?;
        r.SetStreamSelection(MF_SOURCE_READER_ALL_STREAMS.0 as u32, false)
            .map_err(hr("streams"))?;
        Ok(r)
    }
}

/// One `ReadSample`: `None` at the end of the stream.
fn read(r: &IMFSourceReader, stream: u32) -> Result<Option<(i64, Option<IMFSample>)>, String> {
    let (mut flags, mut ts, mut s) = (0u32, 0i64, None);
    // SAFETY: out-pointers to locals.
    unsafe {
        r.ReadSample(
            stream,
            0,
            None,
            Some(&mut flags),
            Some(&mut ts),
            Some(&mut s),
        )
    }
    .map_err(hr("read"))?;
    if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
        return Ok(None);
    }
    Ok(Some((ts, s)))
}

fn bytes_of(s: &IMFSample, mut f: impl FnMut(&[u8])) -> Result<(), String> {
    // SAFETY: the contiguous buffer is locked while `f` reads it.
    unsafe {
        let b = s.ConvertToContiguousBuffer().map_err(hr("buffer"))?;
        let (mut p, mut len) = (std::ptr::null_mut(), 0u32);
        b.Lock(&mut p, None, Some(&mut len)).map_err(hr("lock"))?;
        f(std::slice::from_raw_parts(p, len as usize));
        b.Unlock().map_err(hr("unlock"))?;
    }
    Ok(())
}

/// Read a clip back: size, frame rate, picture count (compressed samples,
/// nothing decoded) and the decoded sound's length.
pub fn probe(path: &Path) -> Result<Probe, String> {
    let _session = Session::start()?;
    let mut p = Probe::default();
    let r = reader(path)?;
    // SAFETY: Media Foundation calls with a live session.
    unsafe {
        r.SetStreamSelection(VIDEO, true)
            .map_err(hr("video stream"))?;
        let t = r.GetCurrentMediaType(VIDEO).map_err(hr("video type"))?;
        let size = t.GetUINT64(&MF_MT_FRAME_SIZE).map_err(hr("frame size"))?;
        (p.width, p.height) = ((size >> 32) as u32, size as u32);
        let rate = t.GetUINT64(&MF_MT_FRAME_RATE).map_err(hr("frame rate"))?;
        p.fps = ((rate >> 32) as u32, rate as u32);
    }
    while let Some((ts, s)) = read(&r, VIDEO)? {
        if let Some(s) = s {
            p.video_start = if p.video_frames == 0 {
                ts
            } else {
                p.video_start.min(ts)
            };
            p.video_frames += 1;
            // SAFETY: a live sample.
            let d = unsafe { s.GetSampleDuration() }.unwrap_or(0);
            p.video_end = p.video_end.max(ts + d);
        }
    }
    let a = decode_audio(path)?;
    (p.audio_rate, p.audio_channels, p.audio_start) = (a.rate, a.channels, a.start);
    p.audio_frames = a.samples.len() as u64 / u64::from(a.channels.max(1));
    Ok(p)
}

/// A clip's sound decoded to 16-bit PCM.
#[derive(Clone, Debug, Default)]
pub struct DecodedAudio {
    pub rate: u32,
    pub channels: u32,
    /// Time of the first decoded sample (100 ns).
    pub start: i64,
    pub samples: Vec<i16>,
}

pub fn decode_audio(path: &Path) -> Result<DecodedAudio, String> {
    let _session = Session::start()?;
    let r = reader(path)?;
    let mut out = DecodedAudio::default();
    // SAFETY: Media Foundation calls with a live session.
    unsafe {
        r.SetStreamSelection(AUDIO, true)
            .map_err(hr("audio stream"))?;
        let pcm = media_type(&MFMediaType_Audio, &MFAudioFormat_PCM)?;
        pcm.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, 16)
            .map_err(hr("bits"))?;
        r.SetCurrentMediaType(AUDIO, None, &pcm)
            .map_err(hr("decode AAC to PCM"))?;
        let t = r.GetCurrentMediaType(AUDIO).map_err(hr("audio type"))?;
        out.rate = t
            .GetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND)
            .map_err(hr("rate"))?;
        out.channels = t
            .GetUINT32(&MF_MT_AUDIO_NUM_CHANNELS)
            .map_err(hr("channels"))?;
    }
    let mut first = true;
    while let Some((ts, s)) = read(&r, AUDIO)? {
        let Some(s) = s else { continue };
        if first {
            out.start = ts;
            first = false;
        }
        bytes_of(&s, |b| {
            out.samples
                .extend(b.chunks_exact(2).map(|c| i16::from_le_bytes([c[0], c[1]])))
        })?;
    }
    Ok(out)
}

/// Decode every picture and hand `f` (index, time in 100 ns, the luma
/// plane's visible rows, stride).
pub fn decode_video_luma(
    path: &Path,
    mut f: impl FnMut(u64, i64, &[u8], usize),
) -> Result<u64, String> {
    decode_video(path, |i, ts, nv12, stride, rows, height| {
        f(i, ts, &nv12[..stride * rows.min(height)], stride)
    })
}

/// Decode every picture to NV12 and hand `f` (index, time in 100 ns, the
/// whole buffer, stride, rows allocated per plane, visible height).
pub fn decode_video(
    path: &Path,
    mut f: impl FnMut(u64, i64, &[u8], usize, usize, usize),
) -> Result<u64, String> {
    let _session = Session::start()?;
    let r = reader(path)?;
    let (width, height);
    // SAFETY: Media Foundation calls with a live session.
    unsafe {
        r.SetStreamSelection(VIDEO, true)
            .map_err(hr("video stream"))?;
        let t = r.GetCurrentMediaType(VIDEO).map_err(hr("video type"))?;
        let size = t.GetUINT64(&MF_MT_FRAME_SIZE).map_err(hr("frame size"))?;
        (width, height) = ((size >> 32) as usize, (size & 0xFFFF_FFFF) as usize);
        let nv12 = media_type(&MFMediaType_Video, &MFVideoFormat_NV12)?;
        r.SetCurrentMediaType(VIDEO, None, &nv12)
            .map_err(hr("decode H.264 to NV12"))?;
    }
    let mut n = 0u64;
    while let Some((ts, s)) = read(&r, VIDEO)? {
        let Some(s) = s else { continue };
        bytes_of(&s, |b| {
            // The decoder may pad rows (1080 -> 1088) and the stride too (1080 wide -> 1088): try
            // the visible height and the height padded to 16 rows.
            let nv12_len = b.len() * 2 / 3;
            let (stride, rows) = [height, height.div_ceil(16) * 16]
                .into_iter()
                .filter(|&r| r > 0 && nv12_len % r == 0 && nv12_len / r >= width)
                .map(|r| (nv12_len / r, r))
                .next()
                .unwrap_or_else(|| {
                    let s = width.max(1);
                    (s, (nv12_len / s).max(1))
                });
            f(n, ts, b, stride, rows, height);
        })?;
        n += 1;
    }
    Ok(n)
}
