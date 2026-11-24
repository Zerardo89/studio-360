use std::{
    fs,
    io::{self, Read, Write},
    os::windows::fs::MetadataExt,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

use image::{ImageFormat, RgbaImage};
use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};
use serde::Deserialize;
use ureq::http::{Response, Uri};
use ureq::tls::{RootCerts, TlsConfig, TlsProvider};

use crate::video::PendingOutput;

const AUDIO_API: &str = "https://api.openverse.org/v1/audio/";
const EMOJI_CDN: &str = "https://cdn.jsdelivr.net/gh/jdecked/twemoji@latest/assets/72x72";
pub const TWEMOJI_CREDIT: &str = "Twemoji © Twitter/X e contributori, CC-BY 4.0";
pub const TWEMOJI_LINK: &str = "https://github.com/jdecked/twemoji";
pub const TWEMOJI_LICENSE: &str = "https://creativecommons.org/licenses/by/4.0/";
pub const MUSIC_ERROR: &str = "Download della musica non riuscito";
pub const TWEMOJI_CREDITS: &str = concat!(
    "Twemoji © Twitter/X e contributori, CC-BY 4.0\n",
    "Fonte: https://github.com/jdecked/twemoji\n",
    "Licenza: https://creativecommons.org/licenses/by/4.0/\n",
    "Modifiche: composizione, ridimensionamento e rotazione.\n",
);
const TIMEOUT: Duration = Duration::from_secs(15);
const SEARCH_LIMIT: u64 = 2 * 1024 * 1024;
const EMOJI_LIMIT: u64 = 64 * 1024;
const MUSIC_LIMIT: u64 = 64 * 1024 * 1024;
const REPARSE_POINT: u32 = 0x400;
static NEXT_SEARCH: Mutex<Option<Instant>> = Mutex::new(None);

#[derive(Clone, Debug)]
pub struct Credit {
    pub title: String,
    pub creator: String,
    pub license: String,
    pub license_url: String,
    pub link: String,
}

impl Credit {
    pub fn caption(&self) -> String {
        format!("{} · {} · {}", self.title, self.creator, self.license)
    }

    pub fn credits(&self) -> String {
        format!("{}\nFonte: {}\nLicenza: {}\nModifiche: selezione, sincronizzazione, mix e dissolvenza.\n",
            self.caption(), self.link, self.license_url)
    }
}

#[derive(Clone)]
pub struct Track {
    pub credit: Credit,
    url: String,
    extension: &'static str,
    pub duration: f64,
}

#[derive(Deserialize)]
struct SearchResults {
    results: Vec<AudioEntry>,
}

#[derive(Deserialize)]
struct AudioEntry {
    title: Option<String>,
    creator: Option<String>,
    license: Option<String>,
    license_version: Option<String>,
    license_url: Option<String>,
    url: Option<String>,
    foreign_landing_url: Option<String>,
    duration: Option<f64>,
    filetype: Option<String>,
}

#[derive(Debug)]
pub enum Error {
    Offline,
    Network,
    RateLimited(u64),
    Http(u16),
    Invalid,
    Cache,
    Cancelled,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Offline => f.write_str("Internet disattivato. Contenuto assente dalla cache."),
            Self::Network => f.write_str("Errore di rete: controlla la connessione"),
            Self::RateLimited(seconds) => {
                write!(f, "Limite Openverse raggiunto. Riprova tra {seconds} s.")
            }
            Self::Http(status) => write!(f, "Il servizio ha risposto con HTTP {status}."),
            Self::Invalid => f.write_str("Il servizio ha restituito un contenuto non valido."),
            Self::Cache => f.write_str("Cache di Studio360 non disponibile."),
            Self::Cancelled => f.write_str("Operazione annullata."),
        }
    }
}

#[derive(Clone)]
pub struct Online(Arc<State>);

struct State {
    agent: ureq::Agent,
    root: Option<PathBuf>,
    enabled: AtomicBool,
    generation: AtomicU64,
    cache_write: Mutex<()>,
}

impl Online {
    pub fn new(enabled: bool) -> Self {
        let root = std::env::var_os("LOCALAPPDATA")
            .filter(|path| !path.is_empty())
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
            .map(|path| path.join("Studio360").join("cache"));
        Self::with_root(enabled, root)
    }

    fn with_root(enabled: bool, root: Option<PathBuf>) -> Self {
        let tls_config = TlsConfig::builder()
            .provider(TlsProvider::NativeTls)
            .root_certs(RootCerts::PlatformVerifier)
            .build();
        let config = ureq::Agent::config_builder()
            .tls_config(tls_config)
            .user_agent("Studio360/0.1 (app desktop)")
            .timeout_global(Some(TIMEOUT))
            .https_only(true)
            .http_status_as_error(false)
            .max_redirects(4)
            .build();
        Self(Arc::new(State {
            agent: config.into(),
            root,
            enabled: AtomicBool::new(enabled),
            generation: AtomicU64::new(0),
            cache_write: Mutex::new(()),
        }))
    }

    #[cfg(test)]
    pub fn without_cache() -> Self {
        Self::with_root(true, None)
    }

    pub fn enabled(&self) -> bool {
        self.0.enabled.load(Ordering::Relaxed)
    }

    pub fn set_enabled(&self, enabled: bool) {
        self.0.enabled.store(enabled, Ordering::Relaxed);
    }

    fn get(&self, url: &str) -> Result<Response<ureq::Body>, Error> {
        if !self.enabled() {
            return Err(Error::Offline);
        }
        self.0.agent.get(url).call().map_err(|_| Error::Network)
    }

    pub fn search(&self, query: &str) -> Result<Vec<Track>, Error> {
        let query: String = query.trim().chars().take(120).collect();
        let encoded = utf8_percent_encode(&query, NON_ALPHANUMERIC);
        let url =
            format!("{AUDIO_API}?q={encoded}&page_size=20&category=music&license=by,by-sa,cc0,pdm");
        let generation = self.generation();
        if let Some(bytes) = self.cached_bytes("music", &url, "json", SEARCH_LIMIT) {
            if let Ok(tracks) = parse_tracks(&bytes) {
                return Ok(tracks);
            }
        }
        if !self.enabled() {
            return Err(Error::Offline);
        }
        // Serialize only Openverse requests. No UI or CDN request waits for this lock.
        let mut next = NEXT_SEARCH.lock().map_err(|_| Error::Network)?;
        if let Some(wait) = next.and_then(|time| time.checked_duration_since(Instant::now())) {
            if wait > Duration::from_secs(1) {
                return Err(Error::RateLimited(wait.as_secs() + 1));
            }
            std::thread::sleep(wait);
        }
        *next = Some(Instant::now() + Duration::from_secs(1));
        let mut response = self.get(&url)?;
        if response.status().as_u16() == 429 {
            let seconds = response
                .headers()
                .get("retry-after")
                .and_then(|value| value.to_str().ok())
                .and_then(|value| value.parse::<u64>().ok())
                .unwrap_or(60)
                .clamp(1, 86_400);
            *next = Some(Instant::now() + Duration::from_secs(seconds));
            return Err(Error::RateLimited(seconds));
        }
        drop(next);
        let bytes = read_response(&mut response, SEARCH_LIMIT)?;
        let tracks = parse_tracks(&bytes)?;
        self.store_bytes("music", &url, "json", &bytes, generation);
        Ok(tracks)
    }

    pub fn emoji(&self, emoji: &str) -> Result<RgbaImage, Error> {
        let urls = emoji_urls(emoji);
        let generation = self.generation();
        for url in &urls {
            if let Some(bytes) = self.cached_bytes("emoji", url, "png", EMOJI_LIMIT) {
                if let Ok(image) = decode_emoji(&bytes) {
                    return Ok(image);
                }
            }
        }
        for url in &urls {
            let mut response = self.get(url)?;
            if response.status().as_u16() == 404 {
                continue;
            }
            let bytes = read_response(&mut response, EMOJI_LIMIT)?;
            let image = decode_emoji(&bytes)?;
            self.store_bytes("emoji", url, "png", &bytes, generation);
            return Ok(image);
        }
        Err(Error::Http(404))
    }

    pub fn download(
        &self,
        track: &Track,
        progress: &AtomicU64,
        cancelled: &AtomicBool,
    ) -> Result<PathBuf, Error> {
        let generation = self.generation();
        let folder = self.folder("music")?;
        let path = folder.join(cache_name(&track.url, track.extension));
        if cached_file(&path, MUSIC_LIMIT) {
            progress.store(100, Ordering::Relaxed);
            return Ok(path);
        }
        let mut response = self.get(&track.url)?;
        let total = response.body().content_length().filter(|&size| size > 0);
        if !response.status().is_success() {
            return Err(Error::Http(response.status().as_u16()));
        }
        if total.is_some_and(|size| size > MUSIC_LIMIT)
            || response.body().mime_type().is_some_and(|mime| {
                mime.starts_with("text/")
                    || mime.starts_with("image/")
                    || mime == "application/json"
            })
        {
            return Err(Error::Invalid);
        }
        let pending = {
            let _guard = self.0.cache_write.lock().map_err(|_| Error::Cache)?;
            if generation != self.generation() {
                return Err(Error::Cancelled);
            }
            fs::create_dir_all(&folder).map_err(|_| Error::Cache)?;
            PendingOutput::new(&path).map_err(|_| Error::Cache)?
        };
        let mut output = fs::File::create(&pending.path).map_err(|_| Error::Cache)?;
        let mut reader = response.body_mut().as_reader();
        let mut buffer = [0; 32 * 1024];
        let mut copied = 0;
        while !cancelled.load(Ordering::Relaxed) && self.enabled() {
            let count = reader.read(&mut buffer).map_err(|_| Error::Network)?;
            if count == 0 {
                break;
            }
            copied += count as u64;
            if copied > MUSIC_LIMIT {
                return Err(Error::Invalid);
            }
            output
                .write_all(&buffer[..count])
                .map_err(|_| Error::Cache)?;
            // Percentages occupy 0..=100. Larger values indicate bytes without a known total.
            let meter = total.map_or(copied + 101, |size| (100 * copied / size).min(99));
            progress.store(meter, Ordering::Relaxed);
        }
        if cancelled.load(Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        if !self.enabled() {
            return Err(Error::Offline);
        }
        if copied == 0 {
            return Err(Error::Invalid);
        }
        output.flush().map_err(|_| Error::Cache)?;
        drop(output);
        let _guard = self.0.cache_write.lock().map_err(|_| Error::Cache)?;
        if generation != self.generation() {
            return Err(Error::Cancelled);
        }
        if !cached_file(&path, MUSIC_LIMIT) {
            pending.commit(&path).map_err(|_| Error::Cache)?;
        }
        progress.store(100, Ordering::Relaxed);
        Ok(path)
    }

    fn generation(&self) -> u64 {
        self.0.generation.load(Ordering::Relaxed)
    }

    fn folder(&self, kind: &str) -> Result<PathBuf, Error> {
        let root = self.cache_root()?;
        let folder = root.join(kind);
        plain_directory(&folder)?;
        Ok(folder)
    }

    fn cache_root(&self) -> Result<&Path, Error> {
        let root = self.0.root.as_deref().ok_or(Error::Cache)?;
        plain_directory(root.parent().ok_or(Error::Cache)?)?;
        plain_directory(root)?;
        Ok(root)
    }

    fn cached_bytes(&self, kind: &str, url: &str, ext: &str, limit: u64) -> Option<Vec<u8>> {
        let path = self.folder(kind).ok()?.join(cache_name(url, ext));
        if !cached_file(&path, limit) {
            return None;
        }
        let mut bytes = Vec::new();
        fs::File::open(path)
            .ok()?
            .take(limit + 1)
            .read_to_end(&mut bytes)
            .ok()?;
        (bytes.len() as u64 <= limit).then_some(bytes)
    }

    fn store_bytes(&self, kind: &str, url: &str, ext: &str, bytes: &[u8], generation: u64) {
        let Ok(_guard) = self.0.cache_write.lock() else {
            return;
        };
        if self.generation() != generation {
            return;
        }
        let Ok(folder) = self.folder(kind) else {
            return;
        };
        let path = folder.join(cache_name(url, ext));
        if fs::create_dir_all(folder).is_ok() {
            if let Ok(pending) = PendingOutput::new(&path) {
                if fs::write(&pending.path, bytes).is_ok() {
                    let _ = pending.commit(&path);
                }
            }
        }
    }

    pub fn cache_size(&self) -> Result<u64, Error> {
        let mut total = 0u64;
        for kind in ["music", "emoji"] {
            let entries = match fs::read_dir(self.folder(kind)?) {
                Ok(entries) => entries,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(_) => return Err(Error::Cache),
            };
            for entry in entries.flatten() {
                if let Ok(metadata) = fs::symlink_metadata(entry.path()) {
                    if metadata.is_file() && metadata.file_attributes() & REPARSE_POINT == 0 {
                        total = total.saturating_add(metadata.len());
                    }
                }
            }
        }
        Ok(total)
    }

    pub fn clear_cache(&self) -> Result<u64, Error> {
        let _guard = self.0.cache_write.lock().map_err(|_| Error::Cache)?;
        let root = self.cache_root()?;
        self.0.generation.fetch_add(1, Ordering::Relaxed);
        match fs::remove_dir_all(root) {
            Ok(()) => Ok(0),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(0),
            Err(_) => Err(Error::Cache),
        }
    }
}

fn plain_directory(path: &Path) -> Result<(), Error> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && metadata.file_attributes() & REPARSE_POINT == 0 => {
            Ok(())
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        _ => Err(Error::Cache),
    }
}

fn cached_file(path: &Path, limit: u64) -> bool {
    fs::symlink_metadata(path).is_ok_and(|m| {
        m.is_file() && m.file_attributes() & REPARSE_POINT == 0 && (1..=limit).contains(&m.len())
    })
}

fn read_response(response: &mut Response<ureq::Body>, limit: u64) -> Result<Vec<u8>, Error> {
    if !response.status().is_success() {
        return Err(Error::Http(response.status().as_u16()));
    }
    let mut bytes = Vec::new();
    response
        .body_mut()
        .as_reader()
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| Error::Network)?;
    if bytes.len() as u64 > limit {
        return Err(Error::Invalid);
    }
    Ok(bytes)
}

fn decode_emoji(bytes: &[u8]) -> Result<RgbaImage, Error> {
    let mut reader = image::ImageReader::with_format(io::Cursor::new(bytes), ImageFormat::Png);
    let mut limits = image::Limits::default();
    limits.max_image_width = Some(72);
    limits.max_image_height = Some(72);
    limits.max_alloc = Some(1024 * 1024);
    reader.limits(limits);
    let image = reader.decode().map_err(|_| Error::Invalid)?.into_rgba8();
    if image.dimensions() != (72, 72) {
        return Err(Error::Invalid);
    }
    Ok(image)
}

fn public_https(url: &str) -> Option<Uri> {
    let uri: Uri = url.parse().ok()?;
    let host = uri.host()?.trim_end_matches('.').to_ascii_lowercase();
    (uri.scheme_str() == Some("https")
        && host.contains('.')
        && host
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
        && host.bytes().any(|b| b.is_ascii_alphabetic())
        && !host.ends_with(".localhost")
        && !host.ends_with(".local")
        && !uri.authority()?.as_str().contains('@')
        && uri.port_u16().is_none_or(|port| port == 443))
    .then_some(uri)
}

pub(crate) fn parse_tracks(json: &[u8]) -> Result<Vec<Track>, Error> {
    let search: SearchResults = serde_json::from_slice(json).map_err(|_| Error::Invalid)?;
    Ok(search
        .results
        .into_iter()
        .take(20)
        .filter_map(|item| {
            let value = item.license?.to_ascii_lowercase();
            let version = item
                .license_version
                .as_deref()
                .filter(|s| matches!(*s, "1.0" | "2.0" | "2.5" | "3.0" | "4.0"));
            let license = match (value.as_str(), version) {
                ("by", _) => "CC BY",
                ("by-sa", _) => "CC BY-SA",
                ("cc0", _) => "CC0",
                ("pdm", _) => "Marchio di pubblico dominio",
                _ => return None,
            };
            let license_url = match value.as_str() {
                "cc0" => "https://creativecommons.org/publicdomain/zero/1.0/".to_owned(),
                "pdm" => "https://creativecommons.org/publicdomain/mark/1.0/".to_owned(),
                _ => version.map_or_else(
                    || "https://creativecommons.org/share-your-work/cclicenses/".to_owned(),
                    |v| format!("https://creativecommons.org/licenses/{value}/{v}/"),
                ),
            };
            let license_url = item
                .license_url
                .filter(|link| {
                    public_https(link).is_some_and(|uri| {
                        uri.host() == Some("creativecommons.org")
                            && match value.as_str() {
                                "cc0" => uri.path().starts_with("/publicdomain/zero/"),
                                "pdm" => uri.path().starts_with("/publicdomain/mark/"),
                                _ => uri.path().starts_with(&format!("/licenses/{value}/")),
                            }
                    })
                })
                .unwrap_or(license_url);
            let license = match version {
                Some(version) => format!("{license} {version}"),
                None => license.to_owned(),
            };
            let url = item.url?;
            let uri = public_https(&url)?;
            let link = item
                .foreign_landing_url
                .filter(|link| public_https(link).is_some())
                .unwrap_or_else(|| url.clone());
            let extension = item
                .filetype
                .as_deref()
                .and_then(audio_extension)
                .or_else(|| {
                    uri.path()
                        .rsplit_once('.')
                        .and_then(|(_, ext)| audio_extension(ext))
                })
                .unwrap_or("mp3");
            let display = |s: Option<String>, fallback: &str| {
                s.map(|s| s.split_whitespace().collect::<Vec<_>>().join(" "))
                    .filter(|s| !s.is_empty())
                    .map_or_else(|| fallback.to_owned(), |s| s.chars().take(180).collect())
            };
            Some(Track {
                credit: Credit {
                    title: display(item.title, "Senza titolo"),
                    creator: display(item.creator, "Autore non indicato"),
                    license,
                    license_url,
                    link,
                },
                duration: item.duration.unwrap_or_default().max(0.0) / 1000.0,
                url,
                extension,
            })
        })
        .collect())
}

fn audio_extension(text: &str) -> Option<&'static str> {
    match text.to_ascii_lowercase().as_str() {
        "mp3" => Some("mp3"),
        "m4a" | "mp4" => Some("m4a"),
        "aac" => Some("aac"),
        "wav" | "wave" => Some("wav"),
        "wma" => Some("wma"),
        "ogg" | "oga" => Some("ogg"),
        "opus" => Some("opus"),
        "flac" => Some("flac"),
        "aiff" | "aif" => Some("aiff"),
        "webm" => Some("webm"),
        _ => None,
    }
}

pub(crate) fn emoji_urls(emoji: &str) -> Vec<String> {
    let mut urls = Vec::new();
    for skip_selector in [false, true] {
        let hex = emoji
            .chars()
            .filter(|&c| !skip_selector || c != '\u{fe0f}')
            .map(|c| format!("{:x}", c as u32))
            .collect::<Vec<_>>()
            .join("-");
        if !hex.is_empty() {
            urls.push(format!("{EMOJI_CDN}/{hex}.png"));
        }
    }
    urls.dedup();
    urls
}

pub(crate) fn cache_name(url: &str, extension: &str) -> String {
    // FNV-1a has a fixed seed and a platform-independent cache key.
    let hash = url.bytes().fold(0xcbf29ce484222325u64, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x100000001b3)
    });
    format!("{hash:016x}.{extension}")
}

pub fn write_credits(destination: &Path, credits: &str) -> Result<(), String> {
    let path = destination.with_extension("crediti.txt");
    let result = (|| {
        let pending = PendingOutput::new(&path).map_err(|e| e.to_string())?;
        fs::write(&pending.path, credits).map_err(|e| e.to_string())?;
        pending.commit(&path).map_err(|e| e.to_string())
    })();
    result.map_err(|error| {
        format!(
            "Impossibile salvare i crediti in {}: {error}",
            path.display()
        )
    })
}
