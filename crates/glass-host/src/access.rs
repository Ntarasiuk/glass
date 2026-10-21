//! Magic links: one-time, short-lived codes that let a device in without the access
//! token. Opening one trades it for a guest pass, a per-device credential that works
//! until it expires or is revoked. Only SHA-256 fingerprints are stored, in a file next
//! to the token, so the CLI and the running host share it and changes apply at once.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Result;
use serde::{Deserialize, Serialize};

/// How long an unopened link stays valid.
pub const LINK_TTL: Duration = Duration::from_secs(15 * 60);
/// Bounds for how long the access a link grants lasts.
pub const MIN_GRANT: Duration = Duration::from_secs(5 * 60);
pub const MAX_GRANT: Duration = Duration::from_secs(30 * 24 * 3600);

/// Serializes read-modify-write within this process (the host's sessions).
static LOCK: Mutex<()> = Mutex::new(());

#[derive(Default, Serialize, Deserialize)]
struct Store {
    #[serde(default)]
    links: Vec<Link>,
    #[serde(default)]
    passes: Vec<Pass>,
}

#[derive(Serialize, Deserialize)]
struct Link {
    hash: String,
    expires: u64,
    grant_secs: u64,
}

#[derive(Serialize, Deserialize)]
struct Pass {
    hash: String,
    expires: u64,
}

/// A link to hand out: the secret code and when it stops working if unopened.
pub struct NewLink {
    pub code: String,
    pub expires: u64,
    pub grant: Duration,
}

/// A guest pass for the device that opened a link.
pub struct Granted {
    pub pass: String,
    pub expires: u64,
}

/// The access file that sits next to the token file.
pub fn file_for(token_file: &Path) -> PathBuf {
    token_file.with_file_name("access.json")
}

pub fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub fn create_link(path: &Path, grant: Duration) -> Result<NewLink> {
    let grant = grant.clamp(MIN_GRANT, MAX_GRANT);
    let code = random_code()?;
    let expires = now() + LINK_TTL.as_secs();
    update(path, |store| {
        store.links.push(Link {
            hash: fingerprint(&code),
            expires,
            grant_secs: grant.as_secs(),
        });
    })?;
    Ok(NewLink {
        code,
        expires,
        grant,
    })
}

/// Uses up a link, if it's valid, and returns a new guest pass in its place.
pub fn redeem_link(path: &Path, code: &str) -> Result<Option<Granted>> {
    let hash = fingerprint(code);
    let pass = random_code()?;
    let mut granted = None;
    update(path, |store| {
        if let Some(i) = store
            .links
            .iter()
            .position(|l| crate::control::token_matches(&l.hash, &hash))
        {
            let link = store.links.remove(i);
            let expires = now() + link.grant_secs;
            store.passes.push(Pass {
                hash: fingerprint(&pass),
                expires,
            });
            granted = Some(Granted {
                pass: pass.clone(),
                expires,
            });
        }
    })?;
    Ok(granted)
}

/// When the pass stops working, if it's valid now.
pub fn pass_expiry(path: &Path, pass: &str) -> Option<u64> {
    let hash = fingerprint(pass);
    let now = now();
    load(path)
        .passes
        .iter()
        .find(|p| p.expires > now && crate::control::token_matches(&p.hash, &hash))
        .map(|p| p.expires)
}

/// Removes every link and pass; returns how many devices lose access.
pub fn revoke_all(path: &Path) -> Result<usize> {
    let mut passes = 0;
    update(path, |store| {
        passes = store.passes.len();
        *store = Store::default();
    })?;
    Ok(passes)
}

fn load(path: &Path) -> Store {
    std::fs::read(path)
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default()
}

/// Loads, drops whatever has expired, applies `f`, and saves atomically with mode 0600.
fn update(path: &Path, f: impl FnOnce(&mut Store)) -> Result<()> {
    let _guard = LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let mut store = load(path);
    let now = now();
    store.links.retain(|l| l.expires > now);
    store.passes.retain(|p| p.expires > now);
    f(&mut store);
    let dir = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir)?;
    let tmp = path.with_extension("tmp");
    let _ = std::fs::remove_file(&tmp);
    {
        use std::io::Write;
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
        let mut file = opts.open(&tmp)?;
        file.write_all(&serde_json::to_vec_pretty(&store)?)?;
        file.sync_all()?;
    }
    std::fs::rename(&tmp, path)?;
    Ok(())
}

fn random_code() -> Result<String> {
    use std::io::Read;
    let mut bytes = [0u8; 16];
    std::fs::File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(hex(&bytes))
}

fn fingerprint(secret: &str) -> String {
    hex(ring::digest::digest(&ring::digest::SHA256, secret.as_bytes()).as_ref())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Parses "30m", "24h", "7d", "2w" (or plain seconds).
pub fn parse_duration(s: &str) -> Result<Duration> {
    let s = s.trim();
    let (num, unit) = s.split_at(s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len()));
    let n: u64 = num
        .parse()
        .map_err(|_| anyhow::anyhow!("expected a duration like 30m, 24h or 7d"))?;
    let secs = match unit {
        "" | "s" => n,
        "m" => n * 60,
        "h" => n * 3600,
        "d" => n * 86400,
        "w" => n * 7 * 86400,
        _ => anyhow::bail!("expected a duration like 30m, 24h or 7d"),
    };
    Ok(Duration::from_secs(secs))
}

/// "24 hours", "7 days", "30 minutes".
pub fn describe(d: Duration) -> String {
    let s = d.as_secs();
    let (n, unit) = if s >= 86400 && s.is_multiple_of(86400) {
        (s / 86400, "day")
    } else if s >= 3600 {
        ((s + 1800) / 3600, "hour")
    } else {
        (s.div_ceil(60), "minute")
    };
    format!("{n} {unit}{}", if n == 1 { "" } else { "s" })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp() -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "glass-access-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir.join("access.json")
    }

    #[test]
    fn a_link_works_once_and_becomes_a_pass() {
        let path = temp();
        let link = create_link(&path, Duration::from_secs(86400)).unwrap();
        let granted = redeem_link(&path, &link.code).unwrap().expect("valid link");
        assert!(
            redeem_link(&path, &link.code).unwrap().is_none(),
            "single use"
        );
        assert_eq!(pass_expiry(&path, &granted.pass), Some(granted.expires));
        assert!(pass_expiry(&path, "not-a-pass").is_none());
        assert!(
            !std::fs::read_to_string(&path)
                .unwrap()
                .contains(&granted.pass),
            "only fingerprints are stored"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        assert_eq!(revoke_all(&path).unwrap(), 1);
        assert!(pass_expiry(&path, &granted.pass).is_none());
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn durations() {
        assert_eq!(parse_duration("30m").unwrap(), Duration::from_secs(1800));
        assert_eq!(
            parse_duration("7d").unwrap(),
            Duration::from_secs(7 * 86400)
        );
        assert!(parse_duration("soon").is_err());
        assert_eq!(describe(Duration::from_secs(86400)), "1 day");
        assert_eq!(describe(Duration::from_secs(7200)), "2 hours");
        assert_eq!(describe(Duration::from_secs(900)), "15 minutes");
        assert_eq!(describe(Duration::from_secs(86399)), "24 hours");
    }
}
