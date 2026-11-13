//! What the viewer remembers for each computer: whether ⌘ is Control there, and whether to
//! play its sound. Kept in `viewer.json` in the config folder.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq)]
pub struct MachinePrefs {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub swap: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio: Option<bool>,
    /// How loud its sound plays here, 0..1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub volume: Option<f32>,
    /// A guest pass a link was traded for (and when it runs out, unix seconds).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pass: Option<(String, u64)>,
}

fn path() -> std::path::PathBuf {
    crate::config_dir().join("viewer.json")
}

/// A pass still good for this computer.
pub fn pass(dns: &str) -> Option<String> {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).ok()?.as_secs();
    load(dns).pass.filter(|(_, exp)| *exp > now).map(|(p, _)| p)
}

fn load_all() -> HashMap<String, MachinePrefs> {
    std::fs::read(path()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

pub fn load(dns: &str) -> MachinePrefs {
    load_all().remove(dns).unwrap_or_default()
}

pub fn update(dns: &str, f: impl FnOnce(&mut MachinePrefs)) {
    let mut all = load_all();
    f(all.entry(dns.to_string()).or_default());
    if let Ok(json) = serde_json::to_vec_pretty(&all) {
        let _ = std::fs::create_dir_all(crate::config_dir());
        let _ = write_private(&path(), &json);
    }
}

fn write_private(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
    opts.open(path)?.write_all(bytes)
}
