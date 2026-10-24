//! Clipboards with more than text: rich text (HTML) and images, both ways. Text and HTML
//! ride the control stream; an image goes in its own `STREAM_CLIPBOARD` stream (a JSON
//! header, then the image bytes) so a big screenshot never holds up the control channel.

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use wtransport::{Connection, RecvStream};

use crate::control::STREAM_CLIPBOARD;

/// Images bigger than this stay where they are.
pub const MAX_IMAGE: usize = 8 << 20;
/// Rich text bigger than this goes as plain text only.
pub const MAX_HTML: usize = 512 << 10;

/// One clipboard's worth: plain text always (maybe empty), plus what else it holds.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Clip {
    pub text: String,
    pub html: Option<String>,
    /// An image as the viewer sent it (PNG, JPEG…), or PNG from the host.
    pub image: Option<Vec<u8>>,
}

/// What precedes the image bytes on a `STREAM_CLIPBOARD` stream.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Header {
    #[serde(default)]
    pub text: String,
    #[serde(default)]
    pub html: Option<String>,
    /// The image's MIME type, e.g. image/png.
    #[serde(default)]
    pub mime: String,
    pub size: u64,
    /// Press Cmd+V once it's set (a paste on the viewer).
    #[serde(default)]
    pub paste: bool,
}

/// Reads one clipboard stream (tag already consumed): the clip, and whether to paste it.
pub async fn receive(stream: &mut RecvStream) -> Result<(Clip, bool)> {
    let mut len = [0u8; 4];
    stream.read_exact(&mut len).await?;
    let len = u32::from_le_bytes(len) as usize;
    if len > MAX_HTML * 2 + (64 << 10) {
        bail!("clipboard header too large");
    }
    let mut header = vec![0u8; len];
    stream.read_exact(&mut header).await?;
    let header: Header = serde_json::from_slice(&header)?;
    if header.size as usize > MAX_IMAGE {
        bail!("clipboard image too large ({} bytes)", header.size);
    }
    let mut image = vec![0u8; header.size as usize];
    stream.read_exact(&mut image).await.context("clipboard image cut short")?;
    let clip = Clip {
        text: header.text,
        html: html_for_viewer(header.html),
        image: (!image.is_empty()).then_some(image),
    };
    Ok((clip, header.paste))
}

/// Sends the host clipboard's image (PNG) to one viewer.
pub async fn send_image(conn: &Connection, png: &[u8]) -> Result<()> {
    let header = serde_json::to_vec(&Header { mime: "image/png".into(), size: png.len() as u64, ..Default::default() })?;
    let mut stream = conn.open_uni().await?.await?;
    let mut prefix = vec![STREAM_CLIPBOARD];
    prefix.extend_from_slice(&(header.len() as u32).to_le_bytes());
    prefix.extend_from_slice(&header);
    stream.write_all(&prefix).await?;
    stream.write_all(png).await?;
    stream.finish().await?;
    Ok(())
}

/// PNG files start with this.
pub fn is_png(bytes: &[u8]) -> bool {
    bytes.starts_with(b"\x89PNG\r\n\x1a\n")
}

/// Rich text worth passing on: not blank and within the size limit.
pub fn html_for_viewer(html: Option<String>) -> Option<String> {
    html.filter(|h| !h.trim().is_empty() && h.len() <= MAX_HTML)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognises_png() {
        assert!(is_png(b"\x89PNG\r\n\x1a\nrest"));
        assert!(!is_png(b"\xff\xd8\xff\xe0 jpeg"));
        assert!(!is_png(b""));
    }

    #[test]
    fn big_or_blank_html_stays_home() {
        assert_eq!(html_for_viewer(Some("<b>hi</b>".into())), Some("<b>hi</b>".into()));
        assert_eq!(html_for_viewer(Some("  ".into())), None);
        assert_eq!(html_for_viewer(Some("x".repeat(MAX_HTML + 1))), None);
        assert_eq!(html_for_viewer(None), None);
    }

    #[test]
    fn a_header_needs_only_a_size() {
        let h: Header = serde_json::from_str(r#"{"size": 3}"#).unwrap();
        assert_eq!((h.size, h.paste, h.text.as_str(), h.html), (3, false, "", None));
    }
}
