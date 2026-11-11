//! Passkeys for links made with `glass link --passkey`, without a browser: this app is its
//! own authenticator. Each passkey is a P-256 key kept in the config folder (mode 0600,
//! like the host's own key), used only after the person at this Mac confirms with Touch ID
//! or their password, so the host sees a verified user (UV) as it would from a browser.
//!
//! The host checks what WebAuthn's browsers send (see `crate::passkey`): clientDataJSON
//! with the challenge and an https origin, the public key as SPKI (create), and an ES256
//! signature over authenticatorData ‖ SHA-256(clientDataJSON) (get).

use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use ring::rand::SystemRandom;
use ring::signature::{ECDSA_P256_SHA256_ASN1_SIGNING, EcdsaKeyPair, KeyPair};
use serde_json::{Value, json};

use crate::remote::{b64url, b64url_decode};

/// DER SubjectPublicKeyInfo header for an uncompressed P-256 key.
const P256_SPKI_PREFIX: [u8; 26] = [
    0x30, 0x59, 0x30, 0x13, 0x06, 0x07, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x02, 0x01, 0x06, 0x08, 0x2a, 0x86, 0x48, 0xce, 0x3d, 0x03, 0x01, 0x07, 0x03, 0x42, 0x00,
];

fn dir() -> PathBuf {
    crate::config_dir().join("viewer-passkeys")
}

fn write_private(path: &std::path::Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    std::fs::create_dir_all(path.parent().unwrap())?;
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut opts, 0o600);
    opts.open(path)?.write_all(bytes)?;
    Ok(())
}

/// Asks the person at this Mac to confirm (Touch ID, or the login password).
#[cfg(target_os = "macos")]
pub async fn confirm(reason: String) -> Result<bool> {
    use block2::RcBlock;
    use objc2::runtime::Bool;
    use objc2_foundation::{NSError, NSString};
    use objc2_local_authentication::{LAContext, LAPolicy};
    let (tx, rx) = tokio::sync::oneshot::channel::<bool>();
    let tx = std::sync::Mutex::new(Some(tx));
    unsafe {
        let ctx = LAContext::new();
        let reply = RcBlock::new(move |ok: Bool, _err: *mut NSError| {
            if let Some(tx) = tx.lock().unwrap().take() {
                let _ = tx.send(ok.as_bool());
            }
        });
        ctx.evaluatePolicy_localizedReason_reply(LAPolicy::DeviceOwnerAuthentication, &NSString::from_str(&reason), &reply);
    }
    if !rx.await.unwrap_or(false) {
        bail!("not confirmed");
    }
    Ok(true)
}

/// Windows Hello (face, fingerprint or PIN). Where it isn't set up, the passkey works
/// without it, as a browser's would, but the host isn't told the user was verified.
#[cfg(windows)]
pub async fn confirm(reason: String) -> Result<bool> {
    use windows::Security::Credentials::UI::{UserConsentVerificationResult as R, UserConsentVerifier};
    // A desktop app asks through the interop, for its window (the one in front: this app's,
    // when someone acts in it; a console's, from `glass viewer-confirm`).
    let result = tokio::task::spawn_blocking(move || -> windows::core::Result<R> {
        use windows_future::IAsyncOperation;
        use windows::Win32::System::Console::GetConsoleWindow;
        use windows::Win32::System::WinRT::IUserConsentVerifierInterop;
        use windows::Win32::UI::WindowsAndMessaging::GetForegroundWindow;
        let message = windows::core::HSTRING::from(reason);
        let interop = windows::core::factory::<UserConsentVerifier, IUserConsentVerifierInterop>()?;
        // SAFETY: plain queries for a window handle.
        let window = unsafe {
            let front = GetForegroundWindow();
            if front.is_invalid() { GetConsoleWindow() } else { front }
        };
        // SAFETY: a window handle and a message string; the operation is joined here.
        let op: IAsyncOperation<R> = unsafe { interop.RequestVerificationForWindowAsync(window, &message)? };
        op.join()
    })
    .await??;
    match result {
        R::Verified => Ok(true),
        R::DeviceNotPresent | R::NotConfiguredForUser | R::DisabledByPolicy => Ok(false),
        _ => bail!("not confirmed"),
    }
}

#[cfg(not(any(target_os = "macos", windows)))]
async fn confirm(_: String) -> Result<bool> {
    Ok(false)
}

fn client_data(kind: &str, challenge: &str, origin: &str) -> Vec<u8> {
    serde_json::to_vec(&json!({ "type": kind, "challenge": challenge, "origin": origin, "crossOrigin": false })).unwrap()
}

/// rpIdHash ‖ flags (user present, and verified when they were) ‖ signCount.
fn authenticator_data(rp_id: &str, counter: u32, verified: bool) -> Vec<u8> {
    let mut out = ring::digest::digest(&ring::digest::SHA256, rp_id.as_bytes()).as_ref().to_vec();
    out.push(0x01 | if verified { 0x04 } else { 0 });
    out.extend_from_slice(&counter.to_be_bytes());
    out
}

/// Answers a host `passkey` message (the `t: passkey` reply), or `mode: error` when it
/// can't (declined, unknown passkey).
pub async fn answer(msg: &Value, origin: &str, computer: &str) -> Value {
    match answer_inner(msg, origin, computer).await {
        Ok(v) => v,
        Err(e) => json!({ "t": "passkey", "mode": "error", "error": format!("{e:#}") }),
    }
}

async fn answer_inner(msg: &Value, origin: &str, computer: &str) -> Result<Value> {
    // A test host on localhost (the only plain-http origin a host accepts, in test mode).
    let test = origin.starts_with("http://localhost");
    let confirm = |reason: String| async move { if test { Ok(true) } else { confirm(reason).await } };
    let challenge = msg["challenge"].as_str().context("no challenge")?;
    let rp_id = origin.split_once("://").map_or(origin, |(_, r)| r).split([':', '/']).next().unwrap_or_default().to_string();
    let rng = SystemRandom::new();
    match msg["mode"].as_str() {
        Some("create") => {
            confirm(format!("make a passkey for {computer}")).await?;
            let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, &rng).map_err(|_| anyhow::anyhow!("couldn't make a key"))?;
            let pair = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, pkcs8.as_ref(), &rng).map_err(|_| anyhow::anyhow!("bad key"))?;
            let mut id = [0u8; 16];
            crate::control::getrandom(&mut id)?;
            let id = b64url(&id);
            write_private(&dir().join(format!("{id}.pk8")), pkcs8.as_ref())?;
            write_private(&dir().join(format!("{id}.count")), b"0")?;
            let mut spki = P256_SPKI_PREFIX.to_vec();
            spki.extend_from_slice(pair.public_key().as_ref());
            Ok(json!({
                "t": "passkey",
                "mode": "create",
                "id": id,
                "client_data": b64url(&client_data("webauthn.create", challenge, origin)),
                "public_key": b64url(&spki),
                "algorithm": -7,
            }))
        }
        Some("get") => {
            let id = msg["credential"].as_str().context("no credential")?;
            // Only an id this app made (no paths).
            if b64url_decode(id).is_none() {
                bail!("bad credential");
            }
            let pkcs8 = std::fs::read(dir().join(format!("{id}.pk8"))).context("this Mac doesn't have that passkey")?;
            let pair = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_ASN1_SIGNING, &pkcs8, &rng).map_err(|_| anyhow::anyhow!("bad key"))?;
            let verified = confirm(format!("sign in to {computer}")).await?;
            let count_path = dir().join(format!("{id}.count"));
            let counter = std::fs::read_to_string(&count_path).ok().and_then(|c| c.trim().parse::<u32>().ok()).unwrap_or(0) + 1;
            write_private(&count_path, counter.to_string().as_bytes())?;
            let auth = authenticator_data(&rp_id, counter, verified);
            let cd = client_data("webauthn.get", challenge, origin);
            let mut signed = auth.clone();
            signed.extend_from_slice(ring::digest::digest(&ring::digest::SHA256, &cd).as_ref());
            let sig = pair.sign(&rng, &signed).map_err(|_| anyhow::anyhow!("signing failed"))?;
            Ok(json!({
                "t": "passkey",
                "mode": "get",
                "id": id,
                "client_data": b64url(&cd),
                "authenticator_data": b64url(&auth),
                "signature": b64url(sig.as_ref()),
            }))
        }
        _ => bail!("unknown passkey request"),
    }
}
