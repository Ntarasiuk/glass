//! Your computers at glassdesk.dev: `glass account add` puts this computer on the account
//! you're signed in to there, through a one-time link; `glass account remove` takes it off
//! every account. The introducer only lists a computer when this host signed the claim
//! with its key, so nobody can add a computer they don't control. An account lists
//! computers and whether they're online; getting in still takes this host's own sign-in.
//!
//! The Glass app can sign in to that account too (`sign_in`): it opens a link in the browser,
//! where you're signed in with your passkey and say yes, then collects a token of its own.
//! That token only lists the account's computers (`computers`).

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
    let mut url = v["url"].as_str().context("no link in the answer")?.to_string();
    // The same link pins the account's passkeys here, so they can open this computer. That
    // part comes after the `#`, which browsers don't send: glassdesk.dev never sees it.
    if !crate::settings::load().account_open_off
        && crate::policy::load().allows("links")
        && let Ok(open) = crate::actions::start_open_enrollment()
    {
        url.push_str(&format!("&open={open}"));
    }
    Ok(Claim {
        url,
        expires: v["expires"].as_u64().unwrap_or_default(),
        name,
    })
}

/// Takes this computer off every account it's on. Whichever passkeys it pinned go first, even
/// if the website can't be reached: it no longer takes commands from what it left.
pub fn remove() -> Result<u64> {
    crate::actions::forget_managers()?;
    crate::actions::forget_openers()?;
    let key = HostKey::load_or_create()?;
    let ts = now();
    let sig = key.sign(format!("glass-unclaim-v1\n{ts}").as_bytes())?;
    let v = post("/api/claims/remove", json!({ "key": key.public(), "ts": ts, "sig": sig }))?;
    Ok(v["removed"].as_u64().unwrap_or_default())
}

pub(crate) fn post(path: &str, body: Value) -> Result<Value> {
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

// ---- The Glass app, signed in ----

/// A computer on the account.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Computer {
    /// The introducer's id for it.
    pub id: String,
    /// Its public key: what a connection from anywhere is made to.
    pub key: String,
    pub name: String,
    pub online: bool,
}

/// A sign-in waiting for the browser to say yes.
pub struct SignIn {
    /// Opens where you're signed in (or sign in, with your passkey).
    pub url: String,
    pub expires: u64,
    secret: String,
    base: String,
}

fn session_file() -> std::path::PathBuf {
    crate::config_dir().join("account.json")
}

/// The site this app is signed in to and its token, if it is. A session made at another
/// site (the relay setting changed since) doesn't count.
fn session() -> Option<(String, String)> {
    let v: Value = serde_json::from_slice(&std::fs::read(session_file()).ok()?).ok()?;
    let (base, token) = (v["base"].as_str()?, v["token"].as_str()?);
    (base == crate::remote::base()).then(|| (base.to_string(), token.to_string()))
}

pub fn signed_in() -> bool {
    session().is_some()
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(10))).http_status_as_error(false).build().new_agent()
}

/// Starts a sign-in: the link to open, good for a few minutes. The link carries only a hash
/// of the secret kept here, so seeing the link isn't enough to collect the token.
pub fn sign_in() -> Result<SignIn> {
    let mut secret = [0u8; 32];
    ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut secret).map_err(|_| anyhow::anyhow!("random"))?;
    let secret = b64url(&secret);
    let code = b64url(ring::digest::digest(&ring::digest::SHA256, format!("glass-app-login-v1\n{secret}").as_bytes()).as_ref());
    let name: String = crate::support::computer_name().chars().take(NAME_MAX).collect();
    let v = post("/api/app/logins", json!({ "code": code, "name": name }))?;
    Ok(SignIn {
        url: v["url"].as_str().context("no link in the answer")?.to_string(),
        expires: v["expires"].as_u64().unwrap_or_default(),
        secret,
        base: crate::remote::base(),
    })
}

impl SignIn {
    /// Whether the browser has said yes: then this app is signed in.
    pub fn poll(&self) -> Result<bool> {
        ensure!(now() < self.expires, "That took too long. Sign in again.");
        // Not reaching the site isn't an answer: asked again, until the link runs out.
        let Ok(mut reply) = agent().post(format!("{}/api/app/token", self.base.trim_end_matches('/'))).send_json(json!({ "secret": self.secret })) else {
            return Ok(false);
        };
        let status = reply.status().as_u16();
        let v: Value = reply.body_mut().read_json().unwrap_or_default();
        if status == 202 {
            return Ok(false);
        }
        ensure!(status == 200, "{}", v["error"].as_str().unwrap_or("the server said no"));
        let token = v["token"].as_str().context("no token in the answer")?;
        write_private(&session_file(), &serde_json::to_vec(&json!({ "base": self.base, "token": token }))?)?;
        Ok(true)
    }
}

/// The account's computers. `None`: not signed in (or signed out from the website since).
pub fn computers() -> Result<Option<Vec<Computer>>> {
    let Some((base, token)) = session() else { return Ok(None) };
    let mut reply = agent()
        .get(format!("{}/api/app/computers", base.trim_end_matches('/')))
        .header("authorization", format!("Bearer {token}"))
        .call()
        .with_context(|| format!("reach {base}"))?;
    let status = reply.status().as_u16();
    let v: Value = reply.body_mut().read_json().unwrap_or_default();
    if status == 401 {
        let _ = std::fs::remove_file(session_file());
        return Ok(None);
    }
    ensure!(status == 200, "{}", v["error"].as_str().unwrap_or("the server said no"));
    let list = v["computers"].as_array().context("no computers in the answer")?;
    Ok(Some(
        list.iter()
            .filter_map(|c| {
                Some(Computer {
                    id: c["id"].as_str()?.to_string(),
                    key: c["key"].as_str()?.to_string(),
                    name: c["name"].as_str()?.to_string(),
                    online: c["online"].as_bool().unwrap_or(false),
                })
            })
            .collect(),
    ))
}

/// Signs this app out. The token is forgotten here even if the website can't be reached.
pub fn sign_out() {
    if let Some((base, token)) = session() {
        let _ = agent().delete(format!("{}/api/app/session", base.trim_end_matches('/'))).header("authorization", format!("Bearer {token}")).call();
    }
    let _ = std::fs::remove_file(session_file());
}

/// This computer's own key, to tell it apart in the account's list.
pub fn own_key() -> Option<String> {
    HostKey::load_or_create().ok().map(|k| k.public())
}

fn write_private(path: &std::path::Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    std::fs::create_dir_all(crate::config_dir())?;
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
    opts.open(path)?.write_all(bytes)?;
    Ok(())
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or_default()
}
