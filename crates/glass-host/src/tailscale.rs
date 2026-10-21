//! Trusts peers that Tailscale says belong to the same user as this Mac, so devices on
//! your own tailnet connect without a token.

use std::net::IpAddr;

use anyhow::{Context, Result};
use serde_json::Value;

/// `tailscale` on PATH, or the CLI inside the Mac App Store / standalone app bundle.
fn cli() -> &'static str {
    const BUNDLED: &str = "/Applications/Tailscale.app/Contents/MacOS/Tailscale";
    static CLI: std::sync::OnceLock<&str> = std::sync::OnceLock::new();
    CLI.get_or_init(|| {
        let on_path = std::process::Command::new("tailscale").arg("version").output().is_ok_and(|o| o.status.success());
        if on_path || !std::path::Path::new(BUNDLED).exists() { "tailscale" } else { BUNDLED }
    })
}

async fn json(args: &[&str]) -> Result<Value> {
    let out = tokio::process::Command::new(cli()).args(args).output().await?;
    anyhow::ensure!(out.status.success(), "tailscale {}: {}", args[0], String::from_utf8_lossy(&out.stderr));
    Ok(serde_json::from_slice(&out.stdout)?)
}

/// The login name of the tailnet user this Mac is signed in as.
pub async fn self_login() -> Result<String> {
    let status = json(&["status", "--json"]).await?;
    let uid = status["Self"]["UserID"].as_u64().context("no Self.UserID")?;
    status["User"][uid.to_string()]["LoginName"]
        .as_str()
        .map(str::to_owned)
        .context("no login name for self")
}

/// A device on the tailnet that belongs to this Mac's owner.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Machine {
    pub name: String,
    /// MagicDNS name, e.g. `macbook.tail1234.ts.net` (no trailing dot).
    pub dns: String,
    pub os: String,
    pub online: bool,
    #[serde(rename = "self")]
    pub is_self: bool,
}

/// This device plus every peer owned by the same user.
pub async fn machines() -> Result<Vec<Machine>> {
    let status = json(&["status", "--json"]).await?;
    let me = status["Self"]["UserID"].as_u64();
    let machine = |node: &Value, is_self: bool| Machine {
        name: node["HostName"].as_str().unwrap_or_default().to_owned(),
        dns: node["DNSName"].as_str().unwrap_or_default().trim_end_matches('.').to_owned(),
        os: node["OS"].as_str().unwrap_or_default().to_owned(),
        online: is_self || node["Online"].as_bool().unwrap_or(false),
        is_self,
    };
    let mut out = vec![machine(&status["Self"], true)];
    if let Some(peers) = status["Peer"].as_object() {
        out.extend(peers.values().filter(|p| p["UserID"].as_u64() == me).map(|p| machine(p, false)));
    }
    // Phones and tablets can view but can't run the host.
    out.retain(|m| !m.dns.is_empty() && !matches!(m.os.as_str(), "iOS" | "android"));
    Ok(out)
}

pub enum Probe {
    Missing,
    LoggedOut,
    /// Connected, but the tailnet hasn't enabled HTTPS certificates.
    NoHttps { name: String },
    Ready { name: String },
}

/// Where this Mac stands with Tailscale, for `glass setup`.
pub async fn probe() -> Probe {
    let Ok(status) = json(&["status", "--json"]).await else {
        let installed = std::process::Command::new(cli()).arg("version").output().is_ok();
        return if installed { Probe::LoggedOut } else { Probe::Missing };
    };
    if status["BackendState"].as_str() != Some("Running") {
        return Probe::LoggedOut;
    }
    let dns = status["Self"]["DNSName"].as_str().unwrap_or_default().trim_end_matches('.').to_owned();
    match status["CertDomains"].as_array().and_then(|d| d.first()).and_then(|d| d.as_str()) {
        Some(name) => Probe::Ready { name: name.to_owned() },
        None => Probe::NoHttps { name: dns },
    }
}

/// Gets (or renews) a Let's Encrypt cert for this Mac's tailnet name via `tailscale cert`.
/// Returns None when Tailscale isn't running or the tailnet hasn't enabled HTTPS.
pub async fn ensure_cert(dir: &std::path::Path) -> Option<(std::path::PathBuf, std::path::PathBuf)> {
    let status = json(&["status", "--json"]).await.ok()?;
    let name = status["CertDomains"].as_array()?.first()?.as_str()?.to_owned();
    std::fs::create_dir_all(dir).ok()?;
    let cert = dir.join(format!("{name}.crt"));
    let key = dir.join(format!("{name}.key"));
    // `tailscale cert` only contacts Let's Encrypt when the cert is missing or near expiry.
    let out = tokio::process::Command::new(cli())
        .arg("cert")
        .arg("--cert-file")
        .arg(&cert)
        .arg("--key-file")
        .arg(&key)
        .arg(&name)
        .output()
        .await
        .ok()?;
    if !out.status.success() {
        tracing::warn!("tailscale cert: {}", String::from_utf8_lossy(&out.stderr).trim());
        return None;
    }
    tracing::info!("using Tailscale HTTPS certificate for {name}");
    Some((cert, key))
}

fn is_tailscale_ip(ip: IpAddr) -> bool {
    match ip {
        // 100.64.0.0/10 (CGNAT range Tailscale assigns from)
        IpAddr::V4(v4) => v4.octets()[0] == 100 && (v4.octets()[1] & 0xC0) == 64,
        // fd7a:115c:a1e0::/48
        IpAddr::V6(v6) => v6.segments()[..3] == [0xfd7a, 0x115c, 0xa1e0],
    }
}

/// True if `peer` is a tailnet device owned by `owner`.
pub async fn is_trusted(peer: IpAddr, owner: &str) -> bool {
    let peer = peer.to_canonical();
    if !is_tailscale_ip(peer) {
        return false;
    }
    match json(&["whois", "--json", &peer.to_string()]).await {
        Ok(who) => who["UserProfile"]["LoginName"].as_str() == Some(owner),
        Err(e) => {
            tracing::warn!("tailscale whois {peer}: {e:#}");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tailscale_ranges() {
        assert!(is_tailscale_ip("100.101.102.103".parse().unwrap()));
        assert!(is_tailscale_ip("fd7a:115c:a1e0::c138:8278".parse().unwrap()));
        assert!(!is_tailscale_ip("100.128.0.1".parse().unwrap()));
        assert!(!is_tailscale_ip("192.168.1.20".parse().unwrap()));
        assert!(is_tailscale_ip("::ffff:100.64.0.7".parse::<IpAddr>().unwrap().to_canonical()));
    }
}
