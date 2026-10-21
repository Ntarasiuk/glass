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

pub fn downloads_dir() -> PathBuf {
    std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default().join("Downloads")
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
        dest.extension().map(|e| format!("{}.", e.to_string_lossy())).unwrap_or_default()
    ));
    let mut file = tokio::fs::File::create(&partial).await?;
    let mut buf = vec![0u8; CHUNK];
    let mut received = 0u64;
    let result: Result<()> = async {
        while received < header.size {
            let want = CHUNK.min((header.size - received) as usize);
            let n = stream.read(&mut buf[..want]).await?.context("upload cut short")?;
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

/// Shows a native file chooser on the Mac's screen (visible to the remote user).
pub async fn pick() -> Result<Vec<PathBuf>> {
    let script = r#"set picked to choose file with prompt "Send to Glass client" with multiple selections allowed
set out to ""
repeat with f in picked
    set out to out & POSIX path of f & linefeed
end repeat
return out"#;
    let output = tokio::process::Command::new("osascript").arg("-e").arg(script).output().await?;
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

pub async fn send(conn: &Connection, path: &Path) -> Result<()> {
    let mut file = tokio::fs::File::open(path).await?;
    let size = file.metadata().await?.len();
    let name = path.file_name().context("no file name")?.to_string_lossy().into_owned();
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
