//! A Unix socket the running host listens on (`glass.sock` in the config dir, owner-only)
//! so the CLI can act on it: answer connection requests, chat, privacy mode, viewers.
//! One JSON request per line, one JSON reply per line.

use std::path::PathBuf;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "cmd", rename_all = "camelCase")]
pub enum Request {
    /// Who's connected.
    Viewers,
    /// End one session, or all of them.
    Disconnect { id: Option<u64> },
    /// Connections waiting to be allowed.
    Pending,
    Decide { id: u64, allow: bool },
    /// A message to every viewer.
    Chat { text: String },
    /// Turn privacy mode on or off for the current sessions.
    Privacy { on: bool },
}

pub fn path() -> PathBuf {
    crate::config_dir().join("glass.sock")
}

/// Serves requests until the host exits.
pub fn serve() -> Result<()> {
    let p = path();
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let _ = std::fs::remove_file(&p);
    let listener = tokio::net::UnixListener::bind(&p).with_context(|| format!("bind {}", p.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600))?;
    }
    tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else { continue };
            tokio::spawn(async move {
                let (read, mut write) = stream.into_split();
                let mut lines = BufReader::new(read).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    let reply = match serde_json::from_str::<Request>(&line) {
                        Ok(req) => handle(req).await,
                        Err(e) => serde_json::json!({ "error": format!("bad request: {e}") }),
                    };
                    let mut out = reply.to_string();
                    out.push('\n');
                    if write.write_all(out.as_bytes()).await.is_err() {
                        break;
                    }
                }
            });
        }
    });
    Ok(())
}

async fn handle(req: Request) -> serde_json::Value {
    use serde_json::json;
    match req {
        Request::Viewers => {
            let list: Vec<_> = crate::viewers::subscribe()
                .borrow()
                .iter()
                .map(|v| json!({ "id": v.id, "describe": v.describe(), "device": v.device }))
                .collect();
            json!({ "viewers": list })
        }
        Request::Disconnect { id } => {
            let n = match id {
                Some(id) => crate::viewers::disconnect(id) as usize,
                None => {
                    let n = crate::viewers::subscribe().borrow().len();
                    crate::viewers::disconnect_all();
                    n
                }
            };
            json!({ "disconnected": n })
        }
        Request::Pending => json!({ "pending": crate::approval::pending() }),
        Request::Decide { id, allow } => json!({ "ok": crate::approval::decide(id, allow) }),
        Request::Chat { text } => match crate::server::host_chat(&text) {
            Ok(n) => json!({ "sent": n }),
            Err(e) => json!({ "error": format!("{e:#}") }),
        },
        Request::Privacy { on } => match crate::privacy::set_forced(on) {
            Ok(()) => json!({ "privacy": on }),
            Err(e) => json!({ "error": format!("{e:#}") }),
        },
    }
}

/// Sends one request to the running host.
pub fn call(req: &Request) -> Result<serde_json::Value> {
    use std::io::{BufRead, Write};
    let mut stream = std::os::unix::net::UnixStream::connect(path())
        .map_err(|_| anyhow::anyhow!("Glass isn't running here. Start it (glass setup), then try again."))?;
    stream.set_read_timeout(Some(std::time::Duration::from_secs(10)))?;
    let mut line = serde_json::to_string(req)?;
    line.push('\n');
    stream.write_all(line.as_bytes())?;
    let mut reply = String::new();
    std::io::BufReader::new(stream).read_line(&mut reply)?;
    let value: serde_json::Value = serde_json::from_str(&reply).context("no reply from Glass")?;
    if let Some(e) = value.get("error").and_then(|e| e.as_str()) {
        anyhow::bail!("{e}");
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_are_one_json_line() {
        let s = serde_json::to_string(&Request::Decide { id: 3, allow: true }).unwrap();
        assert_eq!(s, r#"{"cmd":"decide","id":3,"allow":true}"#);
        let r: Request = serde_json::from_str(r#"{"cmd":"disconnect","id":null}"#).unwrap();
        assert!(matches!(r, Request::Disconnect { id: None }));
    }
}
