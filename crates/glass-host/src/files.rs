//! File transfer over WebTransport unidirectional streams, in both directions.
//! Each stream: `STREAM_FILE` tag, u32 LE header length, JSON [`FileHeader`], file bytes.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tracing::info;
use wtransport::{Connection, RecvStream};

use crate::control::{FileHeader, STREAM_FILE, unique_path};

const MAX_HEADER: usize = 64 << 10;
const CHUNK: usize = 256 << 10;

static DOWNLOADS: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();

pub fn set_downloads_dir(dir: PathBuf) {
    let _ = std::fs::create_dir_all(&dir);
    let _ = DOWNLOADS.set(dir);
}

pub fn downloads_dir() -> PathBuf {
    DOWNLOADS
        .get()
        .cloned()
        .unwrap_or_else(|| crate::home().join("Downloads"))
}

/// Lets the remote user pick files on the host to send to their device.
pub async fn pick() -> Result<Vec<PathBuf>> {
    if crate::testmode::enabled() {
        return Ok(crate::testmode::picked_files());
    }
    pick_native().await
}

/// Receives one uploaded file (tag already consumed) into ~/Downloads.
/// Writes to a temporary name first so a cancelled upload never looks complete.
pub async fn receive(stream: &mut RecvStream) -> Result<(String, PathBuf)> {
    let mut len = [0u8; 4];
    stream.read_exact(&mut len).await?;
    let len = u32::from_le_bytes(len) as usize;
    if len > MAX_HEADER {
        bail!("file header too large");
    }
    let mut header = vec![0u8; len];
    stream.read_exact(&mut header).await?;
    let header: FileHeader = serde_json::from_slice(&header)?;

    let dir = downloads_dir();
    let dest = unique_path(&dir, &header.name);
    let partial = dest.with_extension(format!(
        "{}glass-partial",
        dest.extension()
            .map(|e| format!("{}.", e.to_string_lossy()))
            .unwrap_or_default()
    ));
    let mut file = tokio::fs::File::create(&partial).await?;
    let mut buf = vec![0u8; CHUNK];
    let mut received = 0u64;
    let result: Result<()> = async {
        while received < header.size {
            let want = CHUNK.min((header.size - received) as usize);
            let n = stream
                .read(&mut buf[..want])
                .await?
                .context("upload cut short")?;
            file.write_all(&buf[..n]).await?;
            received += n as u64;
        }
        file.flush().await?;
        Ok(())
    }
    .await;
    if let Err(e) = result {
        let _ = tokio::fs::remove_file(&partial).await;
        return Err(e);
    }
    tokio::fs::rename(&partial, &dest).await?;
    let name = dest.file_name().unwrap().to_string_lossy().into_owned();
    info!("received file {} ({received} bytes)", dest.display());
    Ok((name, dest))
}

/// Shows a native file chooser on the host's screen (visible to the remote user).
#[cfg(target_os = "macos")]
async fn pick_native() -> Result<Vec<PathBuf>> {
    let script = r#"set picked to choose file with prompt "Send to Glass client" with multiple selections allowed
set out to ""
repeat with f in picked
    set out to out & POSIX path of f & linefeed
end repeat
return out"#;
    let output = tokio::process::Command::new("osascript")
        .arg("-e")
        .arg(script)
        .output()
        .await?;
    if !output.status.success() {
        // -128 is "User canceled".
        return Ok(Vec::new());
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter(|l| !l.is_empty())
        .map(PathBuf::from)
        .collect())
}

/// Shows the desktop's file chooser through the portal.
#[cfg(target_os = "linux")]
async fn pick_native() -> Result<Vec<PathBuf>> {
    use ashpd::desktop::file_chooser::SelectedFiles;
    let response = SelectedFiles::open_file()
        .title("Send to Glass client")
        .multiple(true)
        .send()
        .await?
        .response();
    // Cancelled dialogs come back as an error response.
    let Ok(files) = response else {
        return Ok(Vec::new());
    };
    Ok(files
        .uris()
        .iter()
        .filter_map(|u| file_uri_to_path(u.as_str()))
        .collect())
}

/// `file:///home/me/My%20File.txt` → `/home/me/My File.txt`.
#[cfg(any(target_os = "linux", test))]
fn file_uri_to_path(uri: &str) -> Option<PathBuf> {
    let encoded = uri.strip_prefix("file://")?;
    let bytes = encoded.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hex = std::str::from_utf8(&bytes[i + 1..i + 3]).ok()?;
            out.push(u8::from_str_radix(hex, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    use std::os::unix::ffi::OsStringExt;
    Some(PathBuf::from(std::ffi::OsString::from_vec(out)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_file_uris() {
        assert_eq!(
            file_uri_to_path("file:///home/me/My%20File.txt"),
            Some(PathBuf::from("/home/me/My File.txt"))
        );
        assert_eq!(
            file_uri_to_path("file:///tmp/a"),
            Some(PathBuf::from("/tmp/a"))
        );
        assert_eq!(file_uri_to_path("https://x"), None);
    }
}

pub async fn send(conn: &Connection, path: &Path) -> Result<()> {
    let mut file = tokio::fs::File::open(path).await?;
    let size = file.metadata().await?.len();
    let name = path
        .file_name()
        .context("no file name")?
        .to_string_lossy()
        .into_owned();
    let header = serde_json::to_vec(&FileHeader { name, size })?;

    let mut stream = conn.open_uni().await?.await?;
    let mut prefix = vec![STREAM_FILE];
    prefix.extend_from_slice(&(header.len() as u32).to_le_bytes());
    prefix.extend_from_slice(&header);
    stream.write_all(&prefix).await?;

    let mut buf = vec![0u8; CHUNK];
    loop {
        let n = file.read(&mut buf).await?;
        if n == 0 {
            break;
        }
        stream.write_all(&buf[..n]).await?;
    }
    stream.finish().await?;
    info!("sent file {} ({size} bytes)", path.display());
    Ok(())
}
