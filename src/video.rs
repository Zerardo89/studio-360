//! Windows Media Foundation readers and writers. Create, use and drop on one worker.

use std::{
    ffi::c_void,
    fmt,
    marker::PhantomData,
    os::windows::ffi::OsStrExt,
    path::Path,
    ptr,
    rc::Rc,
    slice,
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

use image::RgbaImage;
use windows::{
    core::{s, w, Interface, GUID, HRESULT, PCWSTR},
    Win32::{
        Foundation::{FreeLibrary, HMODULE},
        Media::MediaFoundation::*,
        System::{
            Com::{
                CoInitializeEx, CoUninitialize,
                StructuredStorage::{PropVariantToUInt64, PROPVARIANT},
                COINIT_MULTITHREADED,
            },
            LibraryLoader::{GetProcAddress, LoadLibraryExW, LOAD_LIBRARY_SEARCH_SYSTEM32},
        },
    },
};

const TICKS_PER_SECOND: i64 = 10_000_000;
const DEFAULT_FPS: f64 = 30.0;
pub const AUDIO_HZ: u32 = 48_000;

#[derive(Debug)]
pub struct VideoError {
    message: String,
    #[cfg_attr(not(test), allow(dead_code))]
    unavailable: bool,
}

impl VideoError {
    fn message(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            unavailable: false,
        }
    }

    fn windows(action: &str, error: windows::core::Error) -> Self {
        let code = error.code();
        Self::message(format!(
            "{action} (Media Foundation, 0x{:08X}).",
            code.0 as u32
        ))
    }

    fn platform(error: windows::core::Error) -> Self {
        Self {
            unavailable: true,
            ..Self::windows(
                "Windows Media Foundation non è disponibile. Su Windows N è necessario il Media Feature Pack",
                error,
            )
        }
    }

    fn decode(action: &str, codec: GUID, error: windows::core::Error) -> Self {
        let code = error.code();
        let hevc = codec == MFVideoFormat_HEVC || codec == MFVideoFormat_HEVC_ES;
        if code == MF_E_TOPO_CODEC_NOT_FOUND
            || (hevc
                && matches!(
                    code,
                    MF_E_INVALIDMEDIATYPE | MF_E_TOPO_CANNOT_CONNECT | MF_E_UNSUPPORTED_FORMAT
                ))
        {
            let hint = if hevc {
                " Installa le Estensioni video HEVC di Windows."
            } else {
                " Verifica che il codec video sia installato in Windows."
            };
            let mut failure = Self::windows("Decoder video di Windows non disponibile", error);
            failure.message.push_str(hint);
            return failure;
        }
        Self::windows(action, error)
    }

    fn encode(action: &str, error: windows::core::Error) -> Self {
        let unavailable = matches!(
            error.code(),
            MF_E_TOPO_CODEC_NOT_FOUND | MF_E_PLATFORM_NOT_INITIALIZED
        );
        Self {
            unavailable,
            ..Self::windows(action, error)
        }
    }

    #[cfg(test)]
    pub(crate) fn is_unavailable(&self) -> bool {
        self.unavailable
    }
}

impl fmt::Display for VideoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for VideoError {}

type Result<T> = std::result::Result<T, VideoError>;

type WindowsResult<T> = windows::core::Result<T>;

struct SystemLibrary(HMODULE);

impl SystemLibrary {
    fn load(name: PCWSTR) -> WindowsResult<Self> {
        // System components must never be resolved relative to a media folder.
        unsafe { LoadLibraryExW(name, None, LOAD_LIBRARY_SEARCH_SYSTEM32) }.map(Self)
    }
}

impl Drop for SystemLibrary {
    fn drop(&mut self) {
        let _ = unsafe { FreeLibrary(self.0) };
    }
}

type ComPtr = *mut c_void;
type ComOut = *mut ComPtr;

// Resolve the factories lazily so Windows editions without Media Foundation
// can run the photo editor and report an actionable error on opening a video.
struct MfApi {
    startup: unsafe extern "system" fn(u32, u32) -> HRESULT,
    shutdown: unsafe extern "system" fn() -> HRESULT,
    attributes: unsafe extern "system" fn(ComOut, u32) -> HRESULT,
    file: unsafe extern "system" fn(
        MF_FILE_ACCESSMODE,
        MF_FILE_OPENMODE,
        MF_FILE_FLAGS,
        PCWSTR,
        ComOut,
    ) -> HRESULT,
    media_type: unsafe extern "system" fn(ComOut) -> HRESULT,
    memory_buffer: unsafe extern "system" fn(u32, ComOut) -> HRESULT,
    sample: unsafe extern "system" fn(ComOut) -> HRESULT,
    resolver: unsafe extern "system" fn(ComOut) -> HRESULT,
    stride: unsafe extern "system" fn(u32, u32, *mut i32) -> HRESULT,
    reader_from_url: unsafe extern "system" fn(PCWSTR, ComPtr, ComOut) -> HRESULT,
    reader_from_source: unsafe extern "system" fn(ComPtr, ComPtr, ComOut) -> HRESULT,
    sink_writer: unsafe extern "system" fn(PCWSTR, ComPtr, ComPtr, ComOut) -> HRESULT,
    _readwrite: SystemLibrary,
    _platform: SystemLibrary,
}

impl MfApi {
    fn load() -> WindowsResult<Self> {
        let platform = SystemLibrary::load(w!("mfplat.dll"))?;
        let readwrite = SystemLibrary::load(w!("mfreadwrite.dll"))?;
        macro_rules! export {
            ($dll:ident, $symbol:literal) => {{
                // Each field has the system ABI signature of the named MF export.
                let address = unsafe { GetProcAddress($dll.0, s!($symbol)) }
                    .ok_or_else(windows::core::Error::from_thread)?;
                unsafe { std::mem::transmute(address) }
            }};
        }
        Ok(Self {
            startup: export!(platform, "MFStartup"),
            shutdown: export!(platform, "MFShutdown"),
            attributes: export!(platform, "MFCreateAttributes"),
            file: export!(platform, "MFCreateFile"),
            media_type: export!(platform, "MFCreateMediaType"),
            memory_buffer: export!(platform, "MFCreateMemoryBuffer"),
            sample: export!(platform, "MFCreateSample"),
            resolver: export!(platform, "MFCreateSourceResolver"),
            stride: export!(platform, "MFGetStrideForBitmapInfoHeader"),
            reader_from_url: export!(readwrite, "MFCreateSourceReaderFromURL"),
            reader_from_source: export!(readwrite, "MFCreateSourceReaderFromMediaSource"),
            sink_writer: export!(readwrite, "MFCreateSinkWriterFromURL"),
            _readwrite: readwrite,
            _platform: platform,
        })
    }
}

/// The factory must return an owned COM reference matching T.
unsafe fn create<T: Interface>(factory: impl FnOnce(ComOut) -> HRESULT) -> WindowsResult<T> {
    let mut object = ptr::null_mut();
    factory(&mut object).ok()?;
    if object.is_null() {
        return Err(windows::core::Error::empty());
    }
    Ok(unsafe { T::from_raw(object) })
}

struct MediaFoundation {
    api: MfApi,
    _thread: PhantomData<Rc<()>>,
}

impl MediaFoundation {
    fn start() -> Result<Self> {
        let api = MfApi::load().map_err(VideoError::platform)?;
        unsafe {
            CoInitializeEx(None, COINIT_MULTITHREADED)
                .ok()
                .map_err(|e| VideoError::windows("Impossibile inizializzare COM", e))?;
            if let Err(error) = (api.startup)(MF_VERSION, MFSTARTUP_FULL).ok() {
                CoUninitialize();
                return Err(VideoError::platform(error));
            }
        }
        Ok(Self {
            api,
            _thread: PhantomData,
        })
    }
}

impl Drop for MediaFoundation {
    fn drop(&mut self) {
        unsafe {
            let _ = (self.api.shutdown)().ok();
            CoUninitialize();
        }
    }
}

fn absolute_path(path: &Path) -> Result<Vec<u16>> {
    let absolute = std::path::absolute(path)
        .map_err(|e| VideoError::message(format!("Percorso video non valido: {e}")))?;
    let mut wide: Vec<_> = absolute.as_os_str().encode_wide().collect();
    if wide.contains(&0) {
        return Err(VideoError::message(
            "Il percorso video contiene un carattere non valido.",
        ));
    }
    wide.push(0);
    Ok(wide)
}

fn pair(a: u32, b: u32) -> u64 {
    (u64::from(a) << 32) | u64::from(b)
}

fn unpack(value: u64) -> (u32, u32) {
    ((value >> 32) as u32, value as u32)
}

pub fn is_insv(path: &Path) -> bool {
    path.extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("insv"))
}

#[derive(Clone, Copy)]
pub struct VideoInfo {
    pub duration: f64,
    pub fps: f64,
    pub width: u32,
    pub height: u32,
    pub video_streams: usize,
}

pub struct VideoFrame {
    pub seconds: f64,
    pub rgba: RgbaImage,
}

struct Stream {
    index: u32,
    codec: GUID,
    width: u32,
    height: u32,
    stride: i32,
}

pub struct VideoReader {
    reader: IMFSourceReader,
    streams: Vec<Stream>,
    info: VideoInfo,
    _mf: MediaFoundation,
}

impl VideoReader {
    pub fn open(path: &Path) -> Result<Self> {
        let mf = MediaFoundation::start()?;
        let wide = absolute_path(path)?;
        let insv = is_insv(path);
        let reader = Self::create_reader(&mf.api, PCWSTR(wide.as_ptr()), insv)
            .map_err(|e| VideoError::windows("Impossibile aprire il contenitore video", e))?;
        let mut videos = Vec::new();
        unsafe {
            for index in 0.. {
                let native = match reader.GetNativeMediaType(index, 0) {
                    Ok(native) => native,
                    Err(e) if e.code() == MF_E_INVALIDSTREAMNUMBER => break,
                    Err(e) if e.code() == MF_E_NO_MORE_TYPES => continue,
                    Err(e) => {
                        return Err(VideoError::windows("Impossibile leggere i flussi video", e))
                    }
                };
                if native.GetGUID(&MF_MT_MAJOR_TYPE).ok() == Some(MFMediaType_Video) {
                    videos.push((index, native));
                }
            }
            let Some((_, first)) = videos.first() else {
                return Err(VideoError::message("Il file non contiene un flusso video."));
            };
            let fps = first
                .GetUINT64(&MF_MT_FRAME_RATE)
                .ok()
                .map(unpack)
                .and_then(|(n, d)| (n > 0 && d > 0).then(|| f64::from(n) / f64::from(d)))
                .unwrap_or(DEFAULT_FPS);
            let duration = media_duration(&reader);
            reader
                .SetStreamSelection(MF_SOURCE_READER_ALL_STREAMS.0 as u32, false)
                .map_err(|e| VideoError::windows("Impossibile selezionare i flussi video", e))?;
            let video_streams = videos.len();
            let mut streams = Vec::new();
            for (index, native) in videos.into_iter().take(if insv { 2 } else { 1 }) {
                let codec = native.GetGUID(&MF_MT_SUBTYPE).unwrap_or_default();
                let configure = || {
                    let output: IMFMediaType = create(|out| (mf.api.media_type)(out))?;
                    output.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
                    output.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_RGB32)?;
                    reader.SetStreamSelection(index, true)?;
                    reader.SetCurrentMediaType(index, None, &output)?;
                    Ok(())
                };
                configure().map_err(|e| {
                    VideoError::decode("Impossibile configurare il decoder RGB32", codec, e)
                })?;
                streams.push(Self::layout(&mf.api, &reader, index, codec)?);
            }
            let (width, height) = (streams[0].width, streams[0].height);
            Ok(Self {
                reader,
                streams,
                info: VideoInfo {
                    duration,
                    fps,
                    width,
                    height,
                    video_streams,
                },
                _mf: mf,
            })
        }
    }

    fn create_reader(api: &MfApi, path: PCWSTR, insv: bool) -> WindowsResult<IMFSourceReader> {
        unsafe {
            let attributes: IMFAttributes = create(|out| (api.attributes)(out, 1))?;
            attributes.SetUINT32(&MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, 1)?;
            if !insv {
                return create(|out| (api.reader_from_url)(path, attributes.as_raw(), out));
            }
            // Give the MP4 byte-stream handler a container hint without copying or renaming INSV.
            let bytes: IMFByteStream = create(|out| {
                (api.file)(
                    MF_ACCESSMODE_READ,
                    MF_OPENMODE_FAIL_IF_NOT_EXIST,
                    MF_FILEFLAGS_NONE,
                    path,
                    out,
                )
            })?;
            let resolver: IMFSourceResolver = create(|out| (api.resolver)(out))?;
            let mut kind = MF_OBJECT_INVALID;
            let mut object = None;
            resolver.CreateObjectFromByteStream(
                &bytes,
                w!("video.mp4"),
                MF_RESOLUTION_MEDIASOURCE.0 as u32,
                None,
                &mut kind,
                &mut object,
            )?;
            let source: IMFMediaSource = object.ok_or_else(windows::core::Error::empty)?.cast()?;
            let reader =
                create(|out| (api.reader_from_source)(source.as_raw(), attributes.as_raw(), out));
            if reader.is_err() {
                let _ = source.Shutdown();
            }
            reader
        }
    }

    fn layout(api: &MfApi, reader: &IMFSourceReader, index: u32, codec: GUID) -> Result<Stream> {
        unsafe {
            let media = reader
                .GetCurrentMediaType(index)
                .map_err(|e| VideoError::decode("Formato RGB32 non disponibile", codec, e))?;
            let (width, height) = media
                .GetUINT64(&MF_MT_FRAME_SIZE)
                .map(unpack)
                .map_err(|e| VideoError::windows("Dimensioni del video mancanti", e))?;
            if width == 0
                || height == 0
                || u64::from(width) * u64::from(height) > u64::from(u32::MAX / 4)
            {
                return Err(VideoError::message("Dimensioni del video non valide."));
            }
            let stride = media
                .GetUINT32(&MF_MT_DEFAULT_STRIDE)
                .map(|value| value as i32)
                .or_else(|_| {
                    let mut stride = 0;
                    (api.stride)(MFVideoFormat_RGB32.data1, width, &mut stride)
                        .ok()
                        .map(|_| stride)
                })
                .map_err(|e| VideoError::windows("Passo delle righe RGB32 non valido", e))?;
            Ok(Stream {
                index,
                codec,
                width,
                height,
                stride,
            })
        }
    }

    pub fn info(&self) -> VideoInfo {
        self.info
    }

    /// Media Foundation may start decoding at the preceding keyframe.
    pub fn seek(&mut self, seconds: f64) -> Result<()> {
        if !seconds.is_finite() || seconds < 0.0 {
            return Err(VideoError::message("Posizione del video non valida."));
        }
        let position = PROPVARIANT::from((seconds * TICKS_PER_SECOND as f64).round() as i64);
        unsafe { self.reader.SetCurrentPosition(&GUID::zeroed(), &position) }
            .map_err(|e| VideoError::windows("Impossibile spostarsi nel video", e))
    }

    pub fn next_frame(&mut self) -> Result<Option<VideoFrame>> {
        self.next_on(0)
    }

    pub(crate) fn next_on(&mut self, lens: usize) -> Result<Option<VideoFrame>> {
        let Some(stream) = self.streams.get_mut(lens) else {
            return Ok(None);
        };
        loop {
            let mut sample = None;
            let mut flags = 0;
            let mut timestamp = 0;
            unsafe {
                self.reader.ReadSample(
                    stream.index,
                    0,
                    None,
                    Some(&mut flags),
                    Some(&mut timestamp),
                    Some(&mut sample),
                )
            }
            .map_err(|e| {
                VideoError::decode("Errore durante la decodifica video", stream.codec, e)
            })?;
            if flags & MF_SOURCE_READERF_ERROR.0 as u32 != 0 {
                return Err(VideoError::message(
                    "Il decoder di Windows ha interrotto la lettura del video.",
                ));
            }
            if flags & MF_SOURCE_READERF_CURRENTMEDIATYPECHANGED.0 as u32 != 0 {
                *stream = Self::layout(&self._mf.api, &self.reader, stream.index, stream.codec)?;
            }
            if let Some(sample) = sample {
                return Ok(Some(VideoFrame {
                    seconds: timestamp as f64 / TICKS_PER_SECOND as f64,
                    rgba: rgb32(&sample, stream)?,
                }));
            }
            if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
                return Ok(None);
            }
        }
    }
}

fn rgb32(sample: &IMFSample, stream: &Stream) -> Result<RgbaImage> {
    // Lock calls supply pointers valid only until the corresponding unlock.
    // copy_rgb32 validates all offsets and copies into owned memory before unlocking.
    unsafe {
        let buffer = sample
            .ConvertToContiguousBuffer()
            .map_err(|e| VideoError::windows("Fotogramma RGB32 non accessibile", e))?;
        if let Ok(two_d) = buffer.cast::<IMF2DBuffer2>() {
            let mut scanline = ptr::null_mut();
            let mut start = ptr::null_mut();
            let (mut pitch, mut length) = (0, 0);
            if two_d
                .Lock2DSize(
                    MF2DBuffer_LockFlags_Read,
                    &mut scanline,
                    &mut pitch,
                    &mut start,
                    &mut length,
                )
                .is_ok()
            {
                let offset = scanline.addr().checked_sub(start.addr());
                let result = copy_locked(start, length, offset, pitch, stream);
                let _ = two_d.Unlock2D();
                return result;
            }
        }
        let mut start = ptr::null_mut();
        let mut length = 0;
        buffer
            .Lock(&mut start, None, Some(&mut length))
            .map_err(|e| VideoError::windows("Impossibile leggere il fotogramma RGB32", e))?;
        let offset = if stream.stride >= 0 {
            Some(0)
        } else {
            (stream.height as usize - 1).checked_mul(stream.stride.unsigned_abs() as usize)
        };
        let result = copy_locked(start, length, offset, stream.stride, stream);
        let _ = buffer.Unlock();
        result
    }
}

/// The caller holds the MF lock for the allocation [start, start + length).
unsafe fn copy_locked(
    start: *const u8,
    length: u32,
    offset: Option<usize>,
    pitch: i32,
    stream: &Stream,
) -> Result<RgbaImage> {
    let invalid =
        || VideoError::message("Il decoder ha restituito un fotogramma RGB32 incompleto.");
    let row_size = stream.width as usize * 4;
    let high_row = (stream.height as usize - 1) * pitch.unsigned_abs() as usize;
    let first = offset.ok_or_else(invalid)?;
    let low = if pitch < 0 {
        first.checked_sub(high_row)
    } else {
        Some(first)
    }
    .ok_or_else(invalid)?;
    let end = low
        .checked_add(high_row)
        .and_then(|start| start.checked_add(row_size))
        .ok_or_else(invalid)?;
    if start.is_null() || end > length as usize || (pitch.unsigned_abs() as usize) < row_size {
        return Err(invalid());
    }
    let source = unsafe { slice::from_raw_parts(start, length as usize) };
    let mut rgba = RgbaImage::new(stream.width, stream.height);
    for (y, row) in rgba.chunks_exact_mut(row_size).enumerate() {
        let position = first.wrapping_add_signed(y as isize * pitch as isize);
        for (to, from) in row
            .chunks_exact_mut(4)
            .zip(source[position..position + row_size].chunks_exact(4))
        {
            to.copy_from_slice(&[from[2], from[1], from[0], 255]);
        }
    }
    Ok(rgba)
}

/// H.264 MP4 encoder for reframe exports. The caller must explicitly finish the MP4.
#[allow(dead_code)] // Used by the round-trip test until clip export is added.

const AUDIO_CHANNELS: u32 = 2;
const AUDIO_BITS: u32 = 16;
const AUDIO_BYTES_PER_FRAME: u32 = AUDIO_CHANNELS * AUDIO_BITS / 8;
const AAC_BYTES_PER_SECOND: u32 = 24_000;
pub const AUDIO_BLOCK: usize = 4096;
type Stereo = [i16; 2];

fn media_duration(reader: &IMFSourceReader) -> f64 {
    unsafe {
        reader
            .GetPresentationAttribute(MF_SOURCE_READER_MEDIASOURCE.0 as u32, &MF_PD_DURATION)
            .and_then(|value| PropVariantToUInt64(&value))
            .map_or(0.0, |ticks| ticks as f64 / TICKS_PER_SECOND as f64)
    }
}

struct AudioFormat {
    rate: u32,
    channels: usize,
}

impl AudioFormat {
    fn read(reader: &IMFSourceReader, stream: u32) -> Result<Self> {
        let format = || unsafe {
            let media = reader.GetCurrentMediaType(stream)?;
            Ok((
                media.GetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND)?,
                media.GetUINT32(&MF_MT_AUDIO_NUM_CHANNELS)?,
                media.GetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE)?,
            ))
        };
        let (rate, channels, bits) =
            format().map_err(|e| VideoError::windows("Formato PCM non leggibile", e))?;
        if rate == 0 || channels == 0 || channels > 32 || bits != AUDIO_BITS {
            return Err(VideoError::message("Formato PCM 16 bit non valido."));
        }
        Ok(Self {
            rate,
            channels: channels as usize,
        })
    }
}

#[derive(Default)]
struct AudioPacket {
    start: f64,
    next: u64,
    pcm: Vec<Stereo>,
}

pub struct AudioReader {
    reader: IMFSourceReader,
    stream: u32,
    format: AudioFormat,
    packet: AudioPacket,
    position: Option<u64>,
    eof: bool,
    pub duration: f64,
    _mf: MediaFoundation,
}

impl AudioReader {
    pub fn open(path: &Path) -> Result<Option<Self>> {
        let mf = MediaFoundation::start()?;
        let wide = absolute_path(path)?;
        let reader = VideoReader::create_reader(&mf.api, PCWSTR(wide.as_ptr()), is_insv(path))
            .map_err(|e| VideoError::windows("Impossibile aprire il file audio", e))?;
        let stream = MF_SOURCE_READER_FIRST_AUDIO_STREAM.0 as u32;
        let configure = || unsafe {
            match reader.GetNativeMediaType(stream, 0) {
                Err(e) if matches!(e.code(), MF_E_INVALIDSTREAMNUMBER | MF_E_NO_MORE_TYPES) => {
                    return Ok(false)
                }
                other => {
                    other?;
                }
            }
            reader.SetStreamSelection(MF_SOURCE_READER_ALL_STREAMS.0 as u32, false)?;
            reader.SetStreamSelection(stream, true)?;
            let pcm: IMFMediaType = create(|out| (mf.api.media_type)(out))?;
            pcm.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
            pcm.SetGUID(&MF_MT_SUBTYPE, &MFAudioFormat_PCM)?;
            pcm.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, AUDIO_BITS)?;
            reader.SetCurrentMediaType(stream, None, &pcm)?;
            Ok(true)
        };
        if !configure()
            .map_err(|e| VideoError::windows("Impossibile decodificare l’audio PCM", e))?
        {
            return Ok(None);
        }
        let format = AudioFormat::read(&reader, stream)?;
        Ok(Some(Self {
            duration: media_duration(&reader),
            reader,
            stream,
            format,
            packet: AudioPacket::default(),
            position: None,
            eof: false,
            _mf: mf,
        }))
    }

    /// Fill 48 kHz stereo frames, including silence for timestamp gaps.
    /// The returned count excludes padding after EOF.
    pub fn read_at(&mut self, start: u64, out: &mut [Stereo]) -> Result<usize> {
        out.fill([0, 0]);
        if self.position != Some(start) && (self.position.is_some() || start != 0) {
            let ticks = (i128::from(start) * i128::from(TICKS_PER_SECOND) / i128::from(AUDIO_HZ))
                .min(i128::from(i64::MAX)) as i64;
            unsafe {
                self.reader
                    .SetCurrentPosition(&GUID::zeroed(), &PROPVARIANT::from(ticks))
            }
            .map_err(|e| VideoError::windows("Impossibile spostarsi nell’audio", e))?;
            self.packet = AudioPacket::default();
            self.eof = false;
        }
        let mut offset = 0;
        while offset < out.len() {
            let frame = start.saturating_add(offset as u64);
            if frame >= self.packet.next {
                if self.eof || !self.read_packet()? {
                    break;
                }
                continue;
            }
            let source = (frame as f64 - self.packet.start) * f64::from(self.format.rate)
                / f64::from(AUDIO_HZ);
            if source >= -0.5 && !self.packet.pcm.is_empty() {
                let source = source.max(0.0);
                let left = (source as usize).min(self.packet.pcm.len() - 1);
                let right = (left + 1).min(self.packet.pcm.len() - 1);
                let fraction = source.fract();
                out[offset] = std::array::from_fn(|c| {
                    let a = f64::from(self.packet.pcm[left][c]);
                    let b = f64::from(self.packet.pcm[right][c]);
                    (a + (b - a) * fraction).round() as i16
                });
            }
            offset += 1;
        }
        self.position = Some(start.saturating_add(offset as u64));
        Ok(offset)
    }

    fn read_packet(&mut self) -> Result<bool> {
        loop {
            let (mut flags, mut ticks, mut sample) = (0, 0, None);
            unsafe {
                self.reader.ReadSample(
                    self.stream,
                    0,
                    None,
                    Some(&mut flags),
                    Some(&mut ticks),
                    Some(&mut sample),
                )
            }
            .map_err(|e| VideoError::windows("Errore durante la decodifica audio", e))?;
            if flags & MF_SOURCE_READERF_ERROR.0 as u32 != 0 {
                return Err(VideoError::message(
                    "Windows ha interrotto la decodifica audio.",
                ));
            }
            if flags & MF_SOURCE_READERF_CURRENTMEDIATYPECHANGED.0 as u32 != 0 {
                self.format = AudioFormat::read(&self.reader, self.stream)?;
            }
            self.eof = flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0;
            if let Some(sample) = sample {
                let pcm = unsafe { self.read_pcm(&sample) }
                    .map_err(|e| VideoError::windows("Campione PCM non accessibile", e))?;
                let start = ticks as f64 * f64::from(AUDIO_HZ) / TICKS_PER_SECOND as f64;
                let length = pcm.len() as f64 * f64::from(AUDIO_HZ) / f64::from(self.format.rate);
                self.packet = AudioPacket {
                    start,
                    next: (start + length).round() as u64,
                    pcm,
                };
                return Ok(true);
            }
            if self.eof {
                return Ok(false);
            }
        }
    }

    unsafe fn read_pcm(&self, sample: &IMFSample) -> WindowsResult<Vec<Stereo>> {
        unsafe {
            let buffer = sample.ConvertToContiguousBuffer()?;
            let mut data = ptr::null_mut();
            let mut size = 0;
            buffer.Lock(&mut data, None, Some(&mut size))?;
            let frame_bytes = self.format.channels * 2;
            let pcm = if size == 0 {
                Ok(Vec::new())
            } else if data.is_null() || size as usize % frame_bytes != 0 {
                Err(windows::core::Error::empty())
            } else {
                let bytes = slice::from_raw_parts(data, size as usize);
                Ok(bytes
                    .chunks_exact(frame_bytes)
                    .map(|frame| {
                        let left = i16::from_le_bytes([frame[0], frame[1]]);
                        let right = if self.format.channels > 1 {
                            i16::from_le_bytes([frame[2], frame[3]])
                        } else {
                            left
                        };
                        [left, right]
                    })
                    .collect())
            };
            buffer.Unlock()?;
            pcm
        }
    }
}

pub const MUSIC_PREVIEW_SECONDS: u32 = 20;

pub fn play_music_preview(path: &Path, cancel: &AtomicBool, started: impl FnOnce()) -> Result<()> {
    use windows::Win32::{
        Media::Audio::{
            eMultimedia, eRender, IAudioClient, IAudioRenderClient, IMMDeviceEnumerator,
            MMDeviceEnumerator, AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM,
            AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY, WAVEFORMATEX,
        },
        System::Com::{CoCreateInstance, CLSCTX_ALL},
    };

    let Some(mut reader) = AudioReader::open(path)? else {
        return Err(VideoError::message(
            "Il file selezionato non contiene audio.",
        ));
    };
    let format = WAVEFORMATEX {
        wFormatTag: 1, // PCM signed, little-endian
        nChannels: 2,
        nSamplesPerSec: AUDIO_HZ,
        nAvgBytesPerSec: AUDIO_HZ * 4,
        nBlockAlign: 4,
        wBitsPerSample: 16,
        cbSize: 0,
    };
    // AudioReader keeps COM initialized in this worker. All WASAPI objects and
    // buffers are created, used and released on the same thread.
    let playback = || -> WindowsResult<()> {
        let client: IAudioClient = unsafe {
            let devices: IMMDeviceEnumerator =
                CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL)?;
            devices
                .GetDefaultAudioEndpoint(eRender, eMultimedia)?
                .Activate(CLSCTX_ALL, None)?
        };
        unsafe {
            client.Initialize(
                AUDCLNT_SHAREMODE_SHARED,
                AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY,
                1_000_000,
                0,
                &format,
                None,
            )?;
        }
        let render: IAudioRenderClient = unsafe { client.GetService()? };
        let buffer_size = unsafe { client.GetBufferSize()? };
        let mut pcm = [[0i16; 2]; 4096];
        let max_frames = u64::from(AUDIO_HZ * MUSIC_PREVIEW_SECONDS);
        let mut offset = 0;
        let mut decode_error = None;
        let mut started = Some(started);
        let deadline = Instant::now() + Duration::from_secs(u64::from(MUSIC_PREVIEW_SECONDS) + 3);
        let mut drained = false;
        let result: WindowsResult<()> = (|| {
            while !cancel.load(Ordering::Relaxed) && Instant::now() < deadline {
                let padding = unsafe { client.GetCurrentPadding()? };
                if drained || offset >= max_frames {
                    if padding == 0 {
                        break;
                    }
                } else {
                    let frames = u64::from(buffer_size.saturating_sub(padding))
                        .min(max_frames - offset)
                        .min(pcm.len() as u64) as usize;
                    if frames != 0 {
                        let read = match reader.read_at(offset, &mut pcm[..frames]) {
                            Ok(read) => read,
                            Err(error) => {
                                decode_error = Some(error);
                                break;
                            }
                        };
                        if read > 0 {
                            unsafe {
                                let buffer = render.GetBuffer(read as u32)?;
                                ptr::copy_nonoverlapping(
                                    pcm.as_ptr().cast::<u8>(),
                                    buffer,
                                    read * 4,
                                );
                                render.ReleaseBuffer(read as u32, 0)?;
                            }
                            offset += read as u64;
                            if let Some(started) = started.take() {
                                unsafe { client.Start()? };
                                started();
                            }
                        }
                        drained = read < frames;
                    }
                }
                std::thread::sleep(Duration::from_millis(8));
            }
            Ok(())
        })();
        if started.is_none() {
            let _ = unsafe { client.Stop() };
        }
        result?;
        if let Some(error) = decode_error {
            return Err(windows::core::Error::new(
                HRESULT(0x80004005u32 as i32),
                error.to_string(),
            ));
        }
        if offset == 0 && !cancel.load(Ordering::Relaxed) {
            return Err(windows::core::Error::new(
                HRESULT(0x80004005u32 as i32),
                "Nessun campione audio leggibile.",
            ));
        }
        Ok(())
    };
    playback().map_err(|error| VideoError::windows("Anteprima audio non disponibile", error))
}

unsafe fn make_sink(api: &MfApi, path: PCWSTR) -> WindowsResult<IMFSinkWriter> {
    unsafe {
        let attributes: IMFAttributes = create(|out| (api.attributes)(out, 2))?;
        attributes.SetGUID(&MF_TRANSCODE_CONTAINERTYPE, &MFTranscodeContainerType_MPEG4)?;
        attributes.SetUINT32(&MF_SINK_WRITER_DISABLE_THROTTLING, 1)?;
        create(|out| (api.sink_writer)(path, ptr::null_mut(), attributes.as_raw(), out))
    }
}

struct AacStream {
    index: u32,
    frames: u64,
}

impl AacStream {
    unsafe fn add(api: &MfApi, writer: &IMFSinkWriter) -> WindowsResult<Self> {
        unsafe {
            let media_type = |subtype| -> WindowsResult<IMFMediaType> {
                let media: IMFMediaType = create(|out| (api.media_type)(out))?;
                media.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio)?;
                media.SetGUID(&MF_MT_SUBTYPE, &subtype)?;
                media.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, AUDIO_HZ)?;
                media.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, AUDIO_CHANNELS)?;
                media.SetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE, AUDIO_BITS)?;
                Ok(media)
            };
            let output = media_type(MFAudioFormat_AAC)?;
            output.SetUINT32(&MF_MT_AUDIO_AVG_BYTES_PER_SECOND, AAC_BYTES_PER_SECOND)?;
            output.SetUINT32(&MF_MT_AAC_PAYLOAD_TYPE, 0)?;
            let index = writer.AddStream(&output)?;
            let input = media_type(MFAudioFormat_PCM)?;
            input.SetUINT32(&MF_MT_AUDIO_BLOCK_ALIGNMENT, AUDIO_BYTES_PER_FRAME)?;
            input.SetUINT32(
                &MF_MT_AUDIO_AVG_BYTES_PER_SECOND,
                AUDIO_HZ * AUDIO_BYTES_PER_FRAME,
            )?;
            writer.SetInputMediaType(index, &input, None)?;
            Ok(Self { index, frames: 0 })
        }
    }

    fn write(&mut self, api: &MfApi, writer: &IMFSinkWriter, pcm: &[Stereo]) -> Result<()> {
        if pcm.is_empty() {
            return Ok(());
        }
        let size = u32::try_from(pcm.len())
            .ok()
            .and_then(|len| len.checked_mul(AUDIO_BYTES_PER_FRAME))
            .ok_or_else(|| VideoError::message("Blocco audio troppo grande."))?;
        let timestamp = |frame| {
            (i128::from(frame) * i128::from(TICKS_PER_SECOND) / i128::from(AUDIO_HZ)) as i64
        };
        let end = self.frames + pcm.len() as u64;
        let write = || unsafe {
            let buffer: IMFMediaBuffer = create(|out| (api.memory_buffer)(size, out))?;
            let mut start = ptr::null_mut();
            buffer.Lock(&mut start, None, None)?;
            // Windows is little-endian. Arrays of i16 have no inter-element padding.
            ptr::copy_nonoverlapping(pcm.as_ptr().cast::<u8>(), start, size as usize);
            buffer.Unlock()?;
            buffer.SetCurrentLength(size)?;
            let sample: IMFSample = create(|out| (api.sample)(out))?;
            sample.AddBuffer(&buffer)?;
            sample.SetSampleTime(timestamp(self.frames))?;
            sample.SetSampleDuration(timestamp(end) - timestamp(self.frames))?;
            writer.WriteSample(self.index, &sample)
        };
        write().map_err(|e| VideoError::encode("Impossibile scrivere l’audio AAC", e))?;
        self.frames = end;
        Ok(())
    }
}

// Keep incomplete writes beside the destination and remove them on failure.
pub struct PendingOutput {
    pub path: std::path::PathBuf,
}

impl PendingOutput {
    pub fn new(destination: &Path) -> Result<Self> {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        for _ in 0..32 {
            let nonce = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let extension = destination
                .extension()
                .and_then(|ext| ext.to_str())
                .unwrap_or("mp4");
            let path = destination.with_file_name(format!(
                ".studio360-{}-{nonce}.{extension}",
                std::process::id()
            ));
            if path == destination {
                continue;
            }
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(_) => return Ok(Self { path }),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => {
                    return Err(VideoError::message(format!(
                        "Impossibile creare il file di esportazione: {error}"
                    )))
                }
            }
        }
        Err(VideoError::message(
            "Impossibile riservare un nome per l’esportazione.",
        ))
    }

    pub fn commit(self, destination: &Path) -> Result<()> {
        std::fs::rename(&self.path, destination)
            .map_err(|e| VideoError::message(format!("Impossibile salvare il file esportato: {e}")))
    }
}

impl Drop for PendingOutput {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

pub fn same_file(input: &Path, output: &Path) -> bool {
    input == output
        || std::fs::canonicalize(input)
            .ok()
            .zip(std::fs::canonicalize(output).ok())
            .is_some_and(|(a, b)| a == b)
}

pub fn extract_audio(
    source: &Path,
    destination: &Path,
    cancelled: impl Fn() -> bool,
) -> Result<f64> {
    if same_file(source, destination) {
        return Err(VideoError::message(
            "Scegli un file diverso dal video di origine.",
        ));
    }
    let mut reader = AudioReader::open(source)?
        .ok_or_else(|| VideoError::message("Il video non contiene una traccia audio."))?;
    let pending = PendingOutput::new(destination)?;
    let mf = MediaFoundation::start()?;
    let wide = absolute_path(&pending.path)?;
    let configure = || unsafe {
        let writer = make_sink(&mf.api, PCWSTR(wide.as_ptr()))?;
        let audio = AacStream::add(&mf.api, &writer)?;
        writer.BeginWriting()?;
        Ok((writer, audio))
    };
    let (writer, mut audio) = configure()
        .map_err(|e| VideoError::encode("Impossibile avviare il codificatore AAC", e))?;
    let mut pcm = vec![[0, 0]; AUDIO_BLOCK];
    loop {
        if cancelled() {
            return Err(VideoError::message("Estrazione annullata."));
        }
        let len = reader.read_at(audio.frames, &mut pcm)?;
        if len == 0 {
            break;
        }
        audio.write(&mf.api, &writer, &pcm[..len])?;
    }
    if audio.frames == 0 {
        return Err(VideoError::message(
            "Il video non contiene campioni audio leggibili.",
        ));
    }
    unsafe { writer.Finalize() }
        .map_err(|e| VideoError::encode("Impossibile finalizzare l’audio AAC", e))?;
    drop(writer);
    if cancelled() {
        return Err(VideoError::message("Estrazione annullata."));
    }
    pending.commit(destination)?;
    Ok(audio.frames as f64 / f64::from(AUDIO_HZ))
}

pub struct VideoWriter {
    writer: IMFSinkWriter,
    stream: u32,
    audio: Option<AacStream>,
    size: (u32, u32),
    buffer_size: u32,
    fps: u32,
    frames: i64,
    _mf: MediaFoundation,
}

#[allow(dead_code)]
impl VideoWriter {
    pub fn new(path: &Path, width: u32, height: u32, fps: u32, bitrate: u32) -> Result<Self> {
        Self::with_audio(path, width, height, fps, bitrate, false)
    }

    pub fn with_audio(
        path: &Path,
        width: u32,
        height: u32,
        fps: u32,
        bitrate: u32,
        with_audio: bool,
    ) -> Result<Self> {
        let pixels = u64::from(width) * u64::from(height);
        if pixels == 0
            || pixels > u64::from(u32::MAX / 4)
            || width % 2 != 0
            || height % 2 != 0
            || fps == 0
            || fps > TICKS_PER_SECOND as u32
            || bitrate == 0
        {
            return Err(VideoError::message(
                "Per l’MP4 servono dimensioni pari, frequenza e bitrate validi.",
            ));
        }
        let mf = MediaFoundation::start()?;
        let wide = absolute_path(path)?;
        let configure = || unsafe {
            let writer = make_sink(&mf.api, PCWSTR(wide.as_ptr()))?;
            let output = Self::media_type(&mf.api, MFVideoFormat_H264, width, height, fps)?;
            output.SetUINT32(&MF_MT_AVG_BITRATE, bitrate)?;
            let stream = writer.AddStream(&output)?;
            let input = Self::media_type(&mf.api, MFVideoFormat_NV12, width, height, fps)?;
            input.SetUINT32(&MF_MT_DEFAULT_STRIDE, width)?;
            writer.SetInputMediaType(stream, &input, None)?;
            let audio = with_audio
                .then(|| AacStream::add(&mf.api, &writer))
                .transpose()?;
            writer.BeginWriting()?;
            Ok((writer, stream, audio))
        };
        let (writer, stream, audio) = configure().map_err(|e| {
            VideoError::encode(
                if with_audio {
                    "Impossibile avviare i codificatori H.264 e AAC"
                } else {
                    "Impossibile avviare il codificatore H.264"
                },
                e,
            )
        })?;
        Ok(Self {
            writer,
            stream,
            audio,
            size: (width, height),
            buffer_size: (pixels * 3 / 2) as u32,
            fps,
            frames: 0,
            _mf: mf,
        })
    }

    fn media_type(
        api: &MfApi,
        subtype: GUID,
        width: u32,
        height: u32,
        fps: u32,
    ) -> WindowsResult<IMFMediaType> {
        unsafe {
            let media: IMFMediaType = create(|out| (api.media_type)(out))?;
            media.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
            media.SetGUID(&MF_MT_SUBTYPE, &subtype)?;
            media.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
            media.SetUINT64(&MF_MT_FRAME_SIZE, pair(width, height))?;
            media.SetUINT64(&MF_MT_FRAME_RATE, pair(fps, 1))?;
            media.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, pair(1, 1))?;
            media.SetUINT32(&MF_MT_YUV_MATRIX, MFVideoTransferMatrix_BT709.0 as u32)?;
            media.SetUINT32(&MF_MT_VIDEO_NOMINAL_RANGE, MFNominalRange_16_235.0 as u32)?;
            Ok(media)
        }
    }

    pub fn write_frame(&mut self, rgba: &RgbaImage) -> Result<()> {
        if rgba.dimensions() != self.size {
            return Err(VideoError::message(
                "Le dimensioni del fotogramma non coincidono con quelle dell’MP4.",
            ));
        }
        let timestamp = |frame: i64| {
            (i128::from(frame) * i128::from(TICKS_PER_SECOND) / i128::from(self.fps)) as i64
        };
        let write = || unsafe {
            let buffer: IMFMediaBuffer =
                create(|out| (self._mf.api.memory_buffer)(self.buffer_size, out))?;
            let mut start = ptr::null_mut();
            buffer.Lock(&mut start, None, None)?;
            let bytes = slice::from_raw_parts_mut(start, self.buffer_size as usize);
            rgba_to_nv12(rgba, bytes);
            buffer.Unlock()?;
            buffer.SetCurrentLength(self.buffer_size)?;
            let sample: IMFSample = create(|out| (self._mf.api.sample)(out))?;
            sample.AddBuffer(&buffer)?;
            let time = timestamp(self.frames);
            sample.SetSampleTime(time)?;
            sample.SetSampleDuration(timestamp(self.frames + 1) - time)?;
            self.writer.WriteSample(self.stream, &sample)
        };
        write().map_err(|e| VideoError::encode("Impossibile scrivere il fotogramma H.264", e))?;
        self.frames += 1;
        Ok(())
    }

    pub fn finish(self) -> Result<()> {
        unsafe { self.writer.Finalize() }
            .map_err(|e| VideoError::encode("Impossibile finalizzare l’MP4", e))
    }

    pub fn write_audio(&mut self, pcm: &[Stereo]) -> Result<()> {
        self.audio
            .as_mut()
            .ok_or_else(|| VideoError::message("Flusso AAC mancante."))?
            .write(&self._mf.api, &self.writer, pcm)
    }
}

fn rgba_to_nv12(rgba: &RgbaImage, output: &mut [u8]) {
    let width = rgba.width() as usize;
    let (luma, chroma) = output.split_at_mut(width * rgba.height() as usize);
    let rgb = |pixel: &[u8]| {
        [
            i32::from(pixel[0]),
            i32::from(pixel[1]),
            i32::from(pixel[2]),
        ]
    };
    for (y, rgba) in luma.iter_mut().zip(rgba.chunks_exact(4)) {
        let [r, g, b] = rgb(rgba);
        *y = (16 + ((47 * r + 157 * g + 16 * b + 128) >> 8)) as u8;
    }
    for (luma_rows, uv_row) in rgba
        .chunks_exact(width * 8)
        .zip(chroma.chunks_exact_mut(width))
    {
        let (top, bottom) = luma_rows.split_at(width * 4);
        let pairs = top.chunks_exact(8).zip(bottom.chunks_exact(8));
        for ((top, bottom), uv) in pairs.zip(uv_row.chunks_exact_mut(2)) {
            let [a, b, c, d] = [top, &top[4..], bottom, &bottom[4..]].map(rgb);
            let [r, g, b] = std::array::from_fn(|i| (a[i] + b[i] + c[i] + d[i] + 2) / 4);
            uv[0] = (128 + ((-26 * r - 87 * g + 112 * b + 128) >> 8)) as u8;
            uv[1] = (128 + ((112 * r - 102 * g - 10 * b + 128) >> 8)) as u8;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wav_mono_resamples_to_stereo_and_mixes_with_video_audio(
    ) -> std::result::Result<(), Box<dyn std::error::Error>> {
        use crate::{
            autoedit::{Scene, Segment},
            movie::{self, Movie, Music, OutputProfile},
            panorama::View,
        };
        let dir = Path::new("target").join(format!("studio360-mixer-{}", std::process::id()));
        std::fs::create_dir_all(&dir)?;
        let wav = dir.join("music.wav");
        let src = dir.join("source.mp4");
        let output = dir.join("movie.mp4");
        let result = (|| -> std::result::Result<(), Box<dyn std::error::Error>> {
            let rate = 44_100_u32;
            let samples = rate * 2;
            let mut bytes = Vec::new();
            bytes.extend_from_slice(b"RIFF");
            bytes.extend_from_slice(&(36 + samples * 2).to_le_bytes());
            bytes.extend_from_slice(b"WAVEfmt ");
            bytes.extend_from_slice(&16_u32.to_le_bytes());
            bytes.extend_from_slice(&[1, 0, 1, 0]);
            bytes.extend_from_slice(&rate.to_le_bytes());
            bytes.extend_from_slice(&(rate * 2).to_le_bytes());
            bytes.extend_from_slice(&[2, 0, 16, 0]);
            bytes.extend_from_slice(b"data");
            bytes.extend_from_slice(&(samples * 2).to_le_bytes());
            for i in 0..samples {
                let tone = (f64::from(i) * std::f64::consts::TAU * 400.0 / f64::from(rate)).sin();
                let gain = if i >= rate / 2 { 12_000.0 } else { 0.0 };
                bytes.extend_from_slice(&((tone * gain) as i16).to_le_bytes());
            }
            std::fs::write(&wav, bytes)?;
            let mut writer = match VideoWriter::with_audio(&src, 160, 90, 30, 450_000, true) {
                Err(error) if error.is_unavailable() => {
                    eprintln!("Mixer AAC saltato: {error}");
                    return Ok(());
                }
                result => result?,
            };
            let rgba = RgbaImage::from_pixel(160, 90, image::Rgba([20, 110, 60, 255]));
            for _ in 0..30 {
                writer.write_frame(&rgba)?;
                writer.write_audio(&[[0, 9000]; 1600])?;
            }
            writer.finish()?;
            let mut music = Music::open(wav.clone())?;
            music.start = 0.5;
            let mut raw = AudioReader::open(&wav)?.ok_or("WAV senza audio")?;
            let mut buffer = [[0, 0]; 4_800];
            assert_eq!(raw.read_at(27_000, &mut buffer)?, 4_800);
            assert!(buffer.iter().all(|sample| sample[0] == sample[1]));
            let rising_edges = buffer
                .windows(2)
                .filter(|pair| pair[0][0] < 0 && pair[1][0] >= 0)
                .count();
            assert!((39..=41).contains(&rising_edges), "{rising_edges}");
            let segment = Segment {
                source: src.clone(),
                start: 0.0,
                end: 1.0,
                view: View::default(),
                start_view: View::default(),
                end_view: View::default(),
                score: 100,
                scene: Scene::Nature,
                thumbnail: std::sync::Arc::from([]),
            };
            let mut movie = Movie::from_segment(&segment);
            movie.music = Some(music);
            movie.music_volume = 0.5;
            movie.original_audio_volume = 0.5;
            let profile = OutputProfile {
                resolution: 0,
                aspect: 1,
            };
            assert!(movie::render_movie(&movie, &output, profile, false, |_| {}, || true).is_err());
            assert!(!output.exists());
            let report = movie::render_movie(&movie, &output, profile, false, |_| {}, || false)?;
            assert!(report.warnings.is_empty(), "{:?}", report.warnings);
            let info = VideoReader::open(&output)?.info();
            assert_eq!((info.width, info.height), (720, 1280));
            let mut audio = AudioReader::open(&output)?.ok_or("MP4 esportato senza audio")?;
            assert_eq!(audio.read_at(4800, &mut buffer)?, 4800);
            let average = |channel: usize| {
                buffer
                    .iter()
                    .map(|sample| f64::from(sample[channel]))
                    .sum::<f64>()
                    / buffer.len() as f64
            };
            let peak = buffer
                .iter()
                .map(|sample| i32::from(sample[0]).abs())
                .max()
                .unwrap_or_default();
            assert!(peak > 4000, "{peak}");
            assert!(average(1) - average(0) > 2500.0);
            Ok(())
        })();
        for path in [wav, src, output] {
            if path.is_file() {
                std::fs::remove_file(path)?;
            }
        }
        std::fs::remove_dir(dir)?;
        result
    }

    #[test]
    fn aac_mp4_round_trip_and_extract() -> std::result::Result<(), Box<dyn std::error::Error>> {
        let directory = Path::new("target").join(format!("studio360-audio-{}", std::process::id()));
        std::fs::create_dir_all(&directory)?;
        let mp4 = directory.join("sound.mp4");
        let m4a = directory.join("sound.m4a");
        let result = (|| -> std::result::Result<(), Box<dyn std::error::Error>> {
            let mut writer = match VideoWriter::with_audio(&mp4, 160, 90, 30, 450_000, true) {
                Err(error) if error.is_unavailable() => {
                    eprintln!("Round-trip AAC saltato: {error}");
                    return Ok(());
                }
                result => result?,
            };
            let frame = RgbaImage::from_pixel(160, 90, image::Rgba([16, 80, 170, 255]));
            let pcm: Vec<_> = (0..1600)
                .map(|i| {
                    let tone = (f64::from(i) * std::f64::consts::TAU / 80.0).sin();
                    [(tone * 12_000.0) as i16, (tone * 4_000.0) as i16]
                })
                .collect();
            for _ in 0..30 {
                writer.write_frame(&frame)?;
                writer.write_audio(&pcm)?;
            }
            writer.finish()?;
            extract_audio(&mp4, &m4a, || false)?;
            for path in [&mp4, &m4a] {
                let mut reader = AudioReader::open(path)?.ok_or("Flusso audio mancante")?;
                let mut samples = vec![[0, 0]; 4800];
                assert_eq!(reader.read_at(24_000, &mut samples)?, 4800);
                let energy: [f64; 2] = std::array::from_fn(|c| {
                    samples
                        .iter()
                        .map(|sample| f64::from(sample[c]).abs())
                        .sum::<f64>()
                        / samples.len() as f64
                });
                assert!(
                    energy[0] > 4000.0 && energy[1] > 1300.0 && energy[0] > energy[1] * 2.0,
                    "{energy:?}"
                );
            }
            Ok(())
        })();
        for path in [mp4, m4a] {
            if path.is_file() {
                std::fs::remove_file(path)?;
            }
        }
        std::fs::remove_dir(directory)?;
        result
    }

    #[test]
    fn h264_mp4_round_trip() -> std::result::Result<(), Box<dyn std::error::Error>> {
        let explicit = std::env::var_os("STUDIO360_TEST_MP4").map(std::path::PathBuf::from);
        let directory = std::path::Path::new("target")
            .join(format!("studio360-video-test-{}", std::process::id()));
        std::fs::create_dir_all(&directory)?;
        let path = explicit
            .clone()
            .unwrap_or_else(|| directory.join("sample360.mp4"));
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent)?;
        }
        let result = round_trip(&path).and_then(|available| {
            if available {
                check_camera_files(&path, &directory)?;
            }
            Ok(())
        });
        if explicit.is_none() {
            if path.is_file() {
                std::fs::remove_file(&path)?;
            }
        }
        std::fs::remove_dir(directory)?;
        result
    }

    fn round_trip(path: &Path) -> std::result::Result<bool, Box<dyn std::error::Error>> {
        const WIDTH: u32 = 1024;
        const HEIGHT: u32 = 512;
        const FPS: u32 = 30;
        let mut writer = match VideoWriter::new(path, WIDTH, HEIGHT, FPS, 3_000_000) {
            Err(error) if error.is_unavailable() => {
                eprintln!("Round-trip saltato: {error}");
                return Ok(false);
            }
            result => result?,
        };
        let mut first_mean = [0.0; 3];
        for i in 0..FPS * 2 {
            let frame = image::RgbaImage::from_fn(WIDTH, HEIGHT, |x, y| {
                let center_x = 424 + i * 8;
                let spot = x.abs_diff(center_x).pow(2) + y.abs_diff(HEIGHT / 2).pow(2) < 64 * 64;
                image::Rgba(if spot {
                    [250, 38, 110, 255]
                } else {
                    [
                        (25 + 140 * x / WIDTH) as u8,
                        (35 + 155 * y / HEIGHT) as u8,
                        (150 + 65 * x / WIDTH) as u8,
                        255,
                    ]
                })
            });
            if i == 0 {
                first_mean = mean(&frame);
            }
            writer.write_frame(&frame)?;
        }
        writer.finish()?;
        let mut reader = VideoReader::open(path)?;
        let info = reader.info();
        assert_eq!(
            (info.width, info.height, info.video_streams),
            (WIDTH, HEIGHT, 1)
        );
        assert!(
            (info.duration - 2.0).abs() < 0.12,
            "durata {}",
            info.duration
        );
        assert!((info.fps - f64::from(FPS)).abs() < 0.1);
        let first = reader.next_frame()?.ok_or("Primo fotogramma mancante")?;
        for (actual, expected) in mean(&first.rgba).into_iter().zip(first_mean) {
            assert!(
                (actual - expected).abs() < 18.0,
                "media {actual}, attesa {expected}"
            );
        }
        assert!(first.rgba.get_pixel(512, 50)[1] < first.rgba.get_pixel(512, 450)[1]);
        assert!(first.rgba.get_pixel(424, 256)[0] > 205);
        assert!(first.rgba.get_pixel(896, 256)[0] < 180);
        reader.seek(1.4)?;
        let mut last = first;
        while let Some(frame) = reader.next_frame()? {
            last = frame;
        }
        assert!(
            (1.90..2.05).contains(&last.seconds),
            "ultimo PTS {}",
            last.seconds
        );
        assert!(last.rgba.get_pixel(896, 256)[0] > 205);
        println!(
            "MP4 verificato: {} × {}, {:.2} s, {:.2} fps",
            info.width, info.height, info.duration, info.fps
        );
        Ok(true)
    }

    fn check_camera_files(
        path: &Path,
        directory: &Path,
    ) -> std::result::Result<(), Box<dyn std::error::Error>> {
        use crate::playback::PanoramaVideo;

        let camera = directory.join("camera");
        std::fs::create_dir_all(&camera)?;
        let mov = camera.join("sample360.mov");
        let left = camera.join("VID_demo_00_001.insv");
        let right = camera.join("VID_demo_10_001.insv");
        let variants = [&mov, &left, &right];
        for variant in variants {
            std::fs::copy(path, variant)?;
        }
        let check = || -> std::result::Result<(), Box<dyn std::error::Error>> {
            let mut pair = PanoramaVideo::open(&right, 256)?;
            let (time, frame) = pair.frame_at(1.0)?.ok_or("Seek INSV senza fotogrammi")?;
            assert!((time - 1.0).abs() < 0.06);
            assert_eq!(frame.dimensions(), (256, 128));
            crate::album::check_video_thumbnails(&camera)?;
            crate::playback::check_worker_playback(&mov)?;
            Ok(())
        };
        let result = check();
        for variant in variants {
            std::fs::remove_file(variant)?;
        }
        std::fs::remove_dir(camera)?;
        result
    }

    fn mean(frame: &image::RgbaImage) -> [f64; 3] {
        let mut sums = [0.0; 3];
        for pixel in frame.pixels() {
            for channel in 0..3 {
                sums[channel] += f64::from(pixel[channel]);
            }
        }
        sums.map(|sum| sum / (f64::from(frame.width()) * f64::from(frame.height())))
    }
}
