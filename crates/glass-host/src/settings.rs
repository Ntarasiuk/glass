//! Host settings the owner changes with `glass config`, in `settings.json` next to the
//! token. Read fresh where they matter, so changes apply to the running host at once.

use std::path::PathBuf;

use anyhow::Result;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
#[serde(default)]
pub struct Settings {
    /// Ask the person at the computer before any guest (magic link) connects.
    pub ask_guests: bool,
    /// Ask before anyone connects, the owner's own devices included.
    pub ask_everyone: bool,
    /// Record every session to a video file in `recordings_dir`.
    pub record: bool,
    /// Where recordings go (default ~/Movies/Glass or ~/Videos/Glass).
    pub recordings_dir: Option<PathBuf>,
    /// Blank this computer's screen and block its keyboard and mouse while someone is
    /// connected (privacy mode).
    pub privacy: bool,
    /// Prefer HEVC over H.264 when every viewer can decode it.
    pub hevc: bool,
    /// Local ports on this computer (or reachable from it) a signed-in owner device may
    /// open through Glass, e.g. [3000, 8888]. Empty: port forwarding off.
    pub forward_ports: Vec<u16>,
    /// Name shown in the viewer instead of the computer's (branding).
    pub name: Option<String>,
    /// Accent colour for the viewer, as #rrggbb (branding).
    pub accent: Option<String>,
}

pub fn path() -> PathBuf {
    crate::config_dir().join("settings.json")
}

pub fn load() -> Settings {
    std::fs::read(path())
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

pub fn save(s: &Settings) -> Result<()> {
    let p = path();
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = p.with_extension("tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(s)?)?;
    std::fs::rename(&tmp, &p)?;
    Ok(())
}

/// The keys `glass config` knows, with what they mean.
pub const KEYS: &[(&str, &str)] = &[
    ("ask-guests", "on/off: ask before a guest (magic link) connects"),
    ("ask-everyone", "on/off: ask before anyone connects"),
    ("record", "on/off: record every session"),
    ("recordings-dir", "folder for recordings"),
    ("privacy", "on/off: blank this screen and block local input while viewed"),
    ("hevc", "on/off: use HEVC when every viewer supports it"),
    ("forward-ports", "comma-separated ports owner devices may open, e.g. 3000,8888"),
    ("name", "name shown in the viewer"),
    ("accent", "viewer accent colour, e.g. #ff8800"),
];

fn on_off(v: &str) -> Result<bool> {
    match v.to_ascii_lowercase().as_str() {
        "on" | "true" | "yes" | "1" => Ok(true),
        "off" | "false" | "no" | "0" => Ok(false),
        _ => anyhow::bail!("expected on or off"),
    }
}

fn show_bool(b: bool) -> String {
    if b { "on" } else { "off" }.into()
}

impl Settings {
    pub fn get(&self, key: &str) -> Result<String> {
        Ok(match key {
            "ask-guests" => show_bool(self.ask_guests),
            "ask-everyone" => show_bool(self.ask_everyone),
            "record" => show_bool(self.record),
            "recordings-dir" => recordings_dir(self).display().to_string(),
            "privacy" => show_bool(self.privacy),
            "hevc" => show_bool(self.hevc),
            "forward-ports" => self
                .forward_ports
                .iter()
                .map(|p| p.to_string())
                .collect::<Vec<_>>()
                .join(","),
            "name" => self.name.clone().unwrap_or_default(),
            "accent" => self.accent.clone().unwrap_or_default(),
            _ => anyhow::bail!("unknown setting {key}; see glass config"),
        })
    }

    /// Sets `key` from its text form; an empty value resets optional ones.
    pub fn set(&mut self, key: &str, value: &str) -> Result<()> {
        let value = value.trim();
        match key {
            "ask-guests" => self.ask_guests = on_off(value)?,
            "ask-everyone" => self.ask_everyone = on_off(value)?,
            "record" => self.record = on_off(value)?,
            "recordings-dir" => {
                self.recordings_dir = (!value.is_empty()).then(|| PathBuf::from(value))
            }
            "privacy" => self.privacy = on_off(value)?,
            "hevc" => self.hevc = on_off(value)?,
            "forward-ports" => {
                self.forward_ports = value
                    .split([',', ' '])
                    .filter(|p| !p.is_empty())
                    .map(|p| {
                        p.parse::<u16>()
                            .ok()
                            .filter(|&n| n != 0)
                            .ok_or_else(|| anyhow::anyhow!("{p} isn't a port"))
                    })
                    .collect::<Result<_>>()?
            }
            "name" => {
                let name: String = value.chars().filter(|c| !c.is_control()).take(40).collect();
                self.name = (!name.is_empty()).then_some(name);
            }
            "accent" => {
                anyhow::ensure!(
                    value.is_empty()
                        || (value.len() == 7
                            && value.starts_with('#')
                            && value[1..].bytes().all(|b| b.is_ascii_hexdigit())),
                    "expected a colour like #ff8800"
                );
                self.accent = (!value.is_empty()).then(|| value.to_ascii_lowercase());
            }
            _ => anyhow::bail!("unknown setting {key}; see glass config"),
        }
        Ok(())
    }
}

pub fn recordings_dir(s: &Settings) -> PathBuf {
    s.recordings_dir.clone().unwrap_or_else(|| {
        let movies = if cfg!(target_os = "macos") {
            "Movies"
        } else {
            "Videos"
        };
        crate::home().join(movies).join("Glass")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_and_get_round_trip() {
        let mut s = Settings::default();
        s.set("ask-guests", "on").unwrap();
        s.set("forward-ports", "3000, 8888").unwrap();
        s.set("accent", "#FF8800").unwrap();
        s.set("name", "Studio Mac").unwrap();
        assert!(s.ask_guests);
        assert_eq!(s.forward_ports, vec![3000, 8888]);
        assert_eq!(s.get("forward-ports").unwrap(), "3000,8888");
        assert_eq!(s.get("accent").unwrap(), "#ff8800");
        assert!(s.set("accent", "orange").is_err());
        assert!(s.set("forward-ports", "0").is_err());
        assert!(s.set("record", "maybe").is_err());
        assert!(s.set("nope", "on").is_err());
        s.set("name", "").unwrap();
        assert_eq!(s.name, None);
        // Unknown and missing fields are fine.
        let old: Settings = serde_json::from_str(r#"{"ask_guests":true,"future":1}"#).unwrap();
        assert!(old.ask_guests && !old.record);
    }
}
