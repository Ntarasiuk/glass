//! Wake-on-LAN: hosts publish their MAC addresses in /config.json; a host remembers the
//! ones it sees on other tailnet machines, so it can wake them later from its own LAN.

use std::collections::HashMap;
use std::net::UdpSocket;
use std::path::PathBuf;

use anyhow::{Context, Result};

/// The magic packet: 6 × 0xff, then the MAC 16 times.
pub fn magic_packet(mac: [u8; 6]) -> [u8; 102] {
    let mut p = [0xffu8; 102];
    for i in 0..16 {
        p[6 + i * 6..12 + i * 6].copy_from_slice(&mac);
    }
    p
}

pub fn parse_mac(s: &str) -> Option<[u8; 6]> {
    let parts: Vec<&str> = s.trim().split([':', '-']).collect();
    if parts.len() != 6 {
        return None;
    }
    let mut mac = [0u8; 6];
    for (i, p) in parts.iter().enumerate() {
        mac[i] = u8::from_str_radix(p, 16).ok()?;
    }
    (mac != [0; 6]).then_some(mac)
}

pub fn format_mac(mac: [u8; 6]) -> String {
    mac.iter().map(|b| format!("{b:02x}")).collect::<Vec<_>>().join(":")
}

/// Broadcasts the magic packet on ports 9 and 7.
pub fn wake(mac: [u8; 6]) -> Result<()> {
    if crate::testmode::enabled() {
        crate::testmode::record_wake(&format_mac(mac));
        return Ok(());
    }
    let sock = UdpSocket::bind("0.0.0.0:0")?;
    sock.set_broadcast(true)?;
    let packet = magic_packet(mac);
    for port in [9, 7] {
        sock.send_to(&packet, ("255.255.255.255", port))
            .context("send the wake-up packet")?;
    }
    tracing::info!("sent Wake-on-LAN to {}", format_mac(mac));
    Ok(())
}

/// This computer's wired and Wi-Fi MAC addresses (not virtual or tunnel interfaces).
pub fn own_macs() -> Vec<String> {
    if crate::testmode::enabled() {
        return vec!["02:00:00:00:00:01".into()];
    }
    #[cfg(target_os = "linux")]
    {
        let mut out = Vec::new();
        for entry in std::fs::read_dir("/sys/class/net").into_iter().flatten().flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            // Physical devices have a `device` link; skip lo, tailscale0, docker, bridges…
            if !entry.path().join("device").exists() {
                continue;
            }
            if let Ok(mac) = std::fs::read_to_string(entry.path().join("address"))
                && parse_mac(&mac).is_some()
            {
                let _ = name;
                out.push(mac.trim().to_string());
            }
        }
        out
    }
    #[cfg(target_os = "macos")]
    {
        // `networksetup` lists hardware ports (Ethernet, Wi-Fi, Thunderbolt) with addresses.
        let out = std::process::Command::new("networksetup")
            .arg("-listallhardwareports")
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
            .unwrap_or_default();
        parse_hardware_ports(&out)
    }
}

/// From `networksetup -listallhardwareports`: Ethernet and Wi-Fi addresses.
pub fn parse_hardware_ports(out: &str) -> Vec<String> {
    let mut macs = Vec::new();
    let mut port = String::new();
    for line in out.lines() {
        if let Some(p) = line.strip_prefix("Hardware Port: ") {
            port = p.to_string();
        } else if let Some(a) = line.strip_prefix("Ethernet Address: ")
            && (port.contains("Ethernet") || port.contains("Wi-Fi") || port.contains("LAN"))
            && parse_mac(a).is_some()
        {
            macs.push(a.trim().to_string());
        }
    }
    macs
}

fn peers_file() -> PathBuf {
    crate::config_dir().join("peers.json")
}

/// MAC addresses last seen per tailnet machine (by DNS name).
pub fn known() -> HashMap<String, Vec<String>> {
    std::fs::read(peers_file())
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

pub fn remember(dns: &str, macs: &[String]) {
    if macs.is_empty() {
        return;
    }
    let mut all = known();
    if all.get(dns).is_some_and(|m| m == macs) {
        return;
    }
    all.insert(dns.to_string(), macs.to_vec());
    if let Ok(bytes) = serde_json::to_vec_pretty(&all) {
        let _ = std::fs::write(peers_file(), bytes);
    }
}

/// Wakes a machine by MAC address or by a name this host has seen.
pub fn wake_target(target: &str) -> Result<String> {
    if let Some(mac) = parse_mac(target) {
        wake(mac)?;
        return Ok(format_mac(mac));
    }
    let all = known();
    let t = target.trim_end_matches('.').to_ascii_lowercase();
    let macs = all
        .iter()
        .find(|(dns, _)| {
            let dns = dns.trim_end_matches('.').to_ascii_lowercase();
            dns == t || dns.split('.').next() == Some(t.as_str())
        })
        .map(|(_, m)| m.clone())
        .with_context(|| format!("no MAC address known for {target}; pass one like aa:bb:cc:dd:ee:ff"))?;
    for m in &macs {
        if let Some(mac) = parse_mac(m) {
            wake(mac)?;
        }
    }
    Ok(macs.join(", "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn magic_packet_layout() {
        let mac = parse_mac("aa:bb:cc:dd:ee:ff").unwrap();
        let p = magic_packet(mac);
        assert_eq!(&p[..6], &[0xff; 6]);
        assert_eq!(&p[6..12], &mac);
        assert_eq!(&p[96..102], &mac);
        assert_eq!(parse_mac("AA-BB-CC-DD-EE-FF"), Some(mac));
        assert_eq!(parse_mac("00:00:00:00:00:00"), None);
        assert_eq!(parse_mac("nope"), None);
        assert_eq!(format_mac(mac), "aa:bb:cc:dd:ee:ff");
    }

    #[test]
    fn hardware_ports() {
        let out = "Hardware Port: Ethernet\nDevice: en0\nEthernet Address: 3c:22:fb:00:00:01\n\nHardware Port: Wi-Fi\nDevice: en1\nEthernet Address: 3c:22:fb:00:00:02\n\nHardware Port: Thunderbolt Bridge\nDevice: bridge0\nEthernet Address: N/A\n";
        assert_eq!(parse_hardware_ports(out), vec!["3c:22:fb:00:00:01", "3c:22:fb:00:00:02"]);
    }
}
