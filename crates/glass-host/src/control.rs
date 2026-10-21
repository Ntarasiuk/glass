//! Reliable control channel: one bidirectional WebTransport stream per session
//! carrying length-prefixed JSON messages (u32 LE length, then UTF-8 JSON).
//!
//! The first client message must be `hello` with the access token; until then the
//! session gets no video, audio or input.

use std::path::Path;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use wtransport::{RecvStream, SendStream};

use crate::media::DisplayInfo;

/// Unidirectional stream type tags (first byte of the stream).
pub const STREAM_INPUT: u8 = b'I';
pub const STREAM_FILE: u8 = b'F';

const MAX_MESSAGE: usize = 16 << 20;

#[derive(Debug, Deserialize)]
#[serde(tag = "t", rename_all = "camelCase")]
pub enum ClientMsg {
    Hello { token: Option<String> },
    /// Stream just this display.
    SelectDisplay { id: u32 },
    /// Stream every display side by side.
    ShowAll,
    /// Client clipboard text; with `paste`, the host also presses Cmd+V after setting it.
    Clipboard { text: String, #[serde(default)] paste: bool },
    /// Opens a file chooser on the Mac; chosen files are streamed back.
    PickFiles,
    /// Start or stop sending system audio to this session.
    Audio { on: bool },
    /// Pause video while the client tab is hidden (frees its hardware decoder).
    Video { on: bool },
    /// Install the latest release on the host (it restarts; clients reconnect).
    Update,
    /// Last second's receive stats, for adaptive bitrate.
    NetStats(crate::rate::Report),
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "t", rename_all = "camelCase")]
pub enum ServerMsg {
    /// `streams[i]` is the display carried by video stream `i`.
    Welcome { displays: Vec<DisplayInfo>, streams: Vec<DisplayInfo>, audio: bool, host_os: &'static str },
    Layout { streams: Vec<DisplayInfo> },
    /// Current adaptive target bitrate per stream.
    Bitrate { bps: u32 },
    UpdateAvailable { current: &'static str, version: String },
    Clipboard { text: String },
    FileSaved { name: String, path: String },
    Error { msg: String },
}

/// Header that follows the `STREAM_FILE` tag in either direction.
#[derive(Debug, Serialize, Deserialize)]
pub struct FileHeader {
    pub name: String,
    pub size: u64,
}

pub async fn read_json<T: for<'de> Deserialize<'de>>(stream: &mut RecvStream) -> Result<Option<T>> {
    match read_frame(stream).await? {
        Some(buf) => Ok(Some(serde_json::from_slice(&buf)?)),
        None => Ok(None),
    }
}

/// One length-prefixed message body, or `None` when the stream has ended.
pub async fn read_frame(stream: &mut RecvStream) -> Result<Option<Vec<u8>>> {
    let mut len = [0u8; 4];
    if stream.read_exact(&mut len).await.is_err() {
        return Ok(None);
    }
    let len = u32::from_le_bytes(len) as usize;
    if len > MAX_MESSAGE {
        bail!("control message too large: {len}");
    }
    let mut buf = vec![0u8; len];
    stream.read_exact(&mut buf).await.context("truncated control message")?;
    Ok(Some(buf))
}

pub async fn write_json<T: Serialize>(stream: &mut SendStream, msg: &T) -> Result<()> {
    let body = serde_json::to_vec(msg)?;
    let mut frame = Vec::with_capacity(4 + body.len());
    frame.extend_from_slice(&(body.len() as u32).to_le_bytes());
    frame.extend_from_slice(&body);
    stream.write_all(&frame).await?;
    Ok(())
}

/// Loads the access token, creating a random one on first run.
pub fn load_or_create_token(path: &Path) -> Result<String> {
    if let Ok(token) = std::fs::read_to_string(path) {
        let token = token.trim().to_string();
        if !token.is_empty() {
            return Ok(token);
        }
    }
    let mut bytes = [0u8; 16];
    getrandom(&mut bytes)?;
    let token: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    std::fs::write(path, &token)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(token)
}

/// A token must be long enough not to be guessed and safe to put in a URL (`#t=…`).
pub fn validate_token(token: &str) -> Result<()> {
    anyhow::ensure!(token.len() >= 12, "use at least 12 characters");
    anyhow::ensure!(token.len() <= 128, "use at most 128 characters");
    anyhow::ensure!(
        token.bytes().all(|b| b.is_ascii_alphanumeric() || b"-._~".contains(&b)),
        "use only letters, digits and - . _ ~"
    );
    Ok(())
}

/// Saves a chosen token, readable only by this user. Written to a temporary file and
/// renamed so the running host never reads a half-written token.
pub fn save_token(path: &Path, token: &str) -> Result<()> {
    validate_token(token)?;
    let dir = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir)?;
    let tmp = path.with_extension("tmp");
    let _ = std::fs::remove_file(&tmp); // so the 0600 mode below applies to a fresh file
    {
        use std::io::Write;
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
        let mut file = opts.open(&tmp)?;
        file.write_all(token.as_bytes())?;
        file.sync_all()?;
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}

fn getrandom(buf: &mut [u8]) -> Result<()> {
    use std::io::Read;
    std::fs::File::open("/dev/urandom")?.read_exact(buf)?;
    Ok(())
}

/// Constant-time comparison so the token can't be guessed byte by byte from timing.
pub fn token_matches(given: &str, expected: &str) -> bool {
    given.len() == expected.len()
        && given.bytes().zip(expected.bytes()).fold(0u8, |acc, (a, b)| acc | (a ^ b)) == 0
}

/// Picks a non-existing path in `dir` for `name`, stripping any path components.
pub fn unique_path(dir: &Path, name: &str) -> std::path::PathBuf {
    let name = Path::new(name).file_name().and_then(|n| n.to_str()).unwrap_or("file");
    let name = if name.starts_with('.') || name.is_empty() { "file" } else { name };
    let candidate = dir.join(name);
    if !candidate.exists() {
        return candidate;
    }
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s, format!(".{e}")),
        _ => (name, String::new()),
    };
    (1..)
        .map(|i| dir.join(format!("{stem} ({i}){ext}")))
        .find(|p| !p.exists())
        .unwrap()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_validation() {
        assert!(validate_token("correct-horse.battery_staple~1").is_ok());
        assert!(validate_token("short").is_err());
        assert!(validate_token("has spaces in it!").is_err());
        assert!(validate_token("needs#escaping&in-urls").is_err());
        assert!(validate_token(&"a".repeat(129)).is_err());
    }

    #[test]
    fn saved_token_is_private_and_loaded() {
        let dir = std::env::temp_dir().join(format!("glass-token-test-{}", std::process::id()));
        let path = dir.join("token");
        save_token(&path, "my-chosen-token-123").unwrap();
        assert_eq!(load_or_create_token(&path).unwrap(), "my-chosen-token-123");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        }
        assert!(save_token(&path, "bad").is_err());
        assert_eq!(load_or_create_token(&path).unwrap(), "my-chosen-token-123", "a rejected token changes nothing");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn token_compare() {
        assert!(token_matches("abc123", "abc123"));
        assert!(!token_matches("abc124", "abc123"));
        assert!(!token_matches("abc12", "abc123"));
    }

    #[test]
    fn unique_path_strips_dirs_and_dedupes() {
        let dir = std::env::temp_dir().join(format!("glass-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(unique_path(&dir, "../../etc/passwd"), dir.join("passwd"));
        std::fs::write(dir.join("a.txt"), "").unwrap();
        assert_eq!(unique_path(&dir, "a.txt"), dir.join("a (1).txt"));
        assert_eq!(unique_path(&dir, ".hidden"), dir.join("file"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
