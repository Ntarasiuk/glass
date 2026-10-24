//! Knowing about and installing new releases. The host checks glassdesk.dev/version.json
//! now and then; a client can ask the host to update itself, which re-runs the installer
//! in its own process group so it survives the service restart it triggers.

use std::sync::Mutex;
use std::time::Duration;

const CHECK_EVERY: Duration = Duration::from_secs(6 * 3600);

pub const CURRENT: &str = env!("CARGO_PKG_VERSION");

static LATEST: Mutex<Option<String>> = Mutex::new(None);

/// The site to update from; GLASS_BASE overrides it (as for the installer).
pub fn base_url() -> String {
    std::env::var("GLASS_BASE").unwrap_or_else(|_| "https://glassdesk.dev".into())
}

/// The newest release we know of, if it's newer than this one.
pub fn available() -> Option<String> {
    LATEST.lock().unwrap().clone().filter(|v| newer(v, CURRENT))
}

/// Starts the periodic version check. `GLASS_TEST_LATEST` pins the result (tests).
pub fn watch() {
    if let Ok(v) = std::env::var("GLASS_TEST_LATEST") {
        *LATEST.lock().unwrap() = Some(v);
        return;
    }
    if crate::testmode::enabled() {
        return;
    }
    tokio::spawn(async {
        loop {
            match fetch_latest().await {
                Some(v) => {
                    let mut latest = LATEST.lock().unwrap();
                    if newer(&v, CURRENT) && latest.as_deref() != Some(v.as_str()) {
                        tracing::info!("update available: {CURRENT} → {v}");
                        crate::push::activity("Glass update ready", &format!("Version {v} is out (you have {CURRENT})."), "update");
                    }
                    *latest = Some(v);
                }
                None => tracing::debug!("version check failed"),
            }
            tokio::time::sleep(CHECK_EVERY).await;
        }
    });
}

pub async fn fetch_latest() -> Option<String> {
    let url = format!("{}/version.json", base_url());
    let out = tokio::process::Command::new("curl")
        .args(["-fsS", "--max-time", "10", &url])
        .output()
        .await
        .ok()?;
    let json: serde_json::Value = serde_json::from_slice(&out.stdout).ok()?;
    json["version"].as_str().map(str::to_owned)
}

/// Re-runs the installer in a new process group, detached, so it outlives this process
/// when it restarts the service.
pub fn start_self_update() -> anyhow::Result<()> {
    use std::os::unix::process::CommandExt;
    let log = crate::platform::log_dir().join("update.log");
    let _ = std::fs::create_dir_all(log.parent().unwrap());
    let script = format!("curl -fsSL {base}/install.sh | sh", base = base_url());
    std::process::Command::new("sh")
        .args(["-c", &script])
        .env("GLASS_BASE", base_url())
        .stdin(std::process::Stdio::null())
        .stdout(std::fs::File::create(&log)?)
        .stderr(std::fs::File::options().append(true).open(&log)?)
        .process_group(0)
        .spawn()?;
    tracing::info!("self-update started; log: {}", log.display());
    Ok(())
}

/// `glass update`: the installer, in the foreground.
pub fn run_cli() -> anyhow::Result<()> {
    let status = std::process::Command::new("sh")
        .args(["-c", &format!("curl -fsSL {}/install.sh | sh", base_url())])
        .env("GLASS_BASE", base_url())
        .status()?;
    anyhow::ensure!(status.success(), "the installer failed");
    Ok(())
}

/// Semver-ish comparison of dotted numeric versions ("0.10.1" > "0.9.3").
pub fn newer(a: &str, b: &str) -> bool {
    let parse = |v: &str| {
        v.split(['.', '-'])
            .map(|p| p.parse::<u64>().unwrap_or(0))
            .collect::<Vec<_>>()
    };
    parse(a) > parse(b)
}

#[cfg(test)]
mod tests {
    use super::newer;

    #[test]
    fn compares_versions_numerically() {
        assert!(newer("0.2.0", "0.1.9"));
        assert!(newer("0.10.0", "0.9.9"));
        assert!(newer("1.0.0", "0.99.0"));
        assert!(!newer("0.1.0", "0.1.0"));
        assert!(!newer("0.1.0", "0.2.0"));
    }
}
