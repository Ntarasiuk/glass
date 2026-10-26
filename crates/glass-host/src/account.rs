//! Your computers at glassdesk.dev: `glass account add` puts this computer on the account
//! you're signed in to there, through a one-time link; `glass account remove` takes it off
//! every account. The introducer only lists a computer when this host signed the claim
//! with its key, so nobody can add a computer they don't control. An account lists
//! computers and whether they're online; getting in still takes this host's own sign-in.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};

use crate::remote::{HostKey, b64url};

/// A computer's name, as the account shows it, at most this long.
const NAME_MAX: usize = 64;

pub struct Claim {
    /// Opens where you're signed in and adds this computer, once, within minutes.
    pub url: String,
    pub name: String,
    pub expires: u64,
}

/// A one-time link that adds this computer to an account.
pub fn add() -> Result<Claim> {
    ensure!(
        crate::remote::enabled(),
        "remote access is off, and your account opens computers over it. Turn it on with: glass remote on"
    );
    let key = HostKey::load_or_create()?;
    let name: String = crate::support::computer_name().chars().take(NAME_MAX).collect();
    let mut code = [0u8; 32];
    ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut code).map_err(|_| anyhow::anyhow!("random"))?;
    let code = b64url(&code);
    let ts = now();
    let sig = key.sign(format!("glass-claim-v1\n{code}\n{ts}\n{name}").as_bytes())?;
    let v = post(
        "/api/claims",
        json!({ "key": key.public(), "code": code, "ts": ts, "name": name, "sig": sig }),
    )?;
    Ok(Claim {
        url: v["url"].as_str().context("no link in the answer")?.to_string(),
        expires: v["expires"].as_u64().unwrap_or_default(),
        name,
    })
}

/// Takes this computer off every account it's on.
pub fn remove() -> Result<u64> {
    let key = HostKey::load_or_create()?;
    let ts = now();
    let sig = key.sign(format!("glass-unclaim-v1\n{ts}").as_bytes())?;
    let v = post("/api/claims/remove", json!({ "key": key.public(), "ts": ts, "sig": sig }))?;
    Ok(v["removed"].as_u64().unwrap_or_default())
}

fn post(path: &str, body: Value) -> Result<Value> {
    let base = crate::remote::base();
    let mut reply = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(10)))
        .http_status_as_error(false)
        .build()
        .new_agent()
        .post(format!("{}{path}", base.trim_end_matches('/')))
        .send_json(body)
        .with_context(|| format!("reach {base}"))?;
    let ok = reply.status().is_success();
    let v: Value = reply.body_mut().read_json().unwrap_or_default();
    ensure!(ok, "{}", v["error"].as_str().unwrap_or("the server said no"));
    Ok(v)
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or_default()
}
