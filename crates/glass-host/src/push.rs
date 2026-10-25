//! Web Push: notifications on a viewer's phone, tablet or laptop even when Glass isn't open
//! there (an agent needs you, someone asks to connect, a message). Each browser subscribes
//! once (over its signed-in session); this host signs with its own VAPID key and encrypts
//! every message end to end (RFC 8291), so the push service only relays ciphertext.
//!
//! Subscriptions live in `push.json` in the config dir. Each carries a random `key` the
//! browser's service worker uses to act on a notification (Allow / Deny) without the token.

use std::path::PathBuf;
use std::sync::Mutex;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use ring::rand::{SecureRandom, SystemRandom};
use ring::signature::{ECDSA_P256_SHA256_FIXED_SIGNING, EcdsaKeyPair, KeyPair};
use ring::{aead, agreement, hkdf};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

/// Kept per device; the oldest go first past this.
const MAX_SUBSCRIPTIONS: usize = 64;
/// How long a push service keeps an undelivered message (a sleeping phone).
const TTL_SECS: u64 = 12 * 3600;

/// What a device asked to hear about.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
pub struct Kinds {
    /// Chat messages.
    #[serde(default = "yes")]
    pub chat: bool,
    /// Things waiting for an answer: an agent's question, a connection request. Owner only.
    #[serde(default = "yes")]
    pub asks: bool,
    /// Someone joined, a recording was saved, an update is ready. Owner only.
    #[serde(default)]
    pub activity: bool,
}

fn yes() -> bool {
    true
}

impl Default for Kinds {
    fn default() -> Self {
        Self { chat: true, asks: true, activity: false }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Kind {
    Chat,
    Ask,
    Activity,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Subscription {
    pub id: String,
    pub endpoint: String,
    /// The browser's P-256 public key and auth secret, base64url.
    pub p256dh: String,
    pub auth: String,
    /// Proves a notification action came from this device's service worker.
    pub key: String,
    /// "Safari on iPad".
    pub device: String,
    /// The page to open from a notification.
    pub url: String,
    /// Signed in as the computer's owner (not a guest link).
    pub owner: bool,
    pub kinds: Kinds,
    pub created: u64,
    /// Notifications since this device last looked (its home-screen badge).
    #[serde(default)]
    pub unread: u32,
}

impl Subscription {
    fn wants(&self, kind: Kind) -> bool {
        match kind {
            Kind::Chat => self.kinds.chat,
            Kind::Ask => self.owner && self.kinds.asks,
            Kind::Activity => self.owner && self.kinds.activity,
        }
    }
}

#[derive(Default, Serialize, Deserialize)]
struct Store {
    #[serde(default)]
    subscriptions: Vec<Subscription>,
}

static LOCK: Mutex<()> = Mutex::new(());

fn file() -> PathBuf {
    crate::config_dir().join("push.json")
}

fn load() -> Store {
    std::fs::read(file()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
}

fn save(store: &Store) -> Result<()> {
    crate::settings::write_private(&file(), &serde_json::to_vec_pretty(store)?)
}

fn update<T>(f: impl FnOnce(&mut Store) -> T) -> Result<T> {
    let _guard = LOCK.lock().unwrap();
    let mut store = load();
    let out = f(&mut store);
    save(&store)?;
    Ok(out)
}

pub fn list() -> Vec<Subscription> {
    load().subscriptions
}

/// What a browser sends to subscribe (its `PushSubscription`, plus what it wants).
#[derive(Debug, Clone, Deserialize)]
pub struct NewSubscription {
    pub endpoint: String,
    pub p256dh: String,
    pub auth: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub kinds: Kinds,
}

/// Adds (or refreshes) a device's subscription; returns its action key.
pub fn subscribe(new: NewSubscription, device: &str, owner: bool) -> Result<String> {
    check_endpoint(&new.endpoint)?;
    let p256dh = B64.decode(new.p256dh.trim_end_matches('=')).context("p256dh")?;
    let auth = B64.decode(new.auth.trim_end_matches('=')).context("auth")?;
    if p256dh.len() != 65 || auth.len() != 16 {
        bail!("not a push subscription");
    }
    // The page that subscribed (https, or this computer itself), for opening from a notification.
    let local = ["http://localhost", "http://127.0.0.1"].iter().any(|p| new.url.strip_prefix(p).is_some_and(|r| r.starts_with([':', '/'])));
    let url = if new.url.starts_with("https://") || local { new.url } else { String::new() };
    update(|s| {
        // Same endpoint: the same browser, re-subscribing (keep its key and badge).
        let old = s.subscriptions.iter().position(|x| x.endpoint == new.endpoint).map(|i| s.subscriptions.remove(i));
        let key = old.as_ref().map(|o| o.key.clone()).unwrap_or_else(|| random_id(24));
        s.subscriptions.push(Subscription {
            id: old.as_ref().map(|o| o.id.clone()).unwrap_or_else(|| random_id(6)),
            endpoint: new.endpoint,
            p256dh: new.p256dh,
            auth: new.auth,
            key: key.clone(),
            device: device.chars().take(80).collect(),
            url,
            owner,
            kinds: new.kinds,
            created: crate::access::now(),
            unread: old.map_or(0, |o| o.unread),
        });
        let extra = s.subscriptions.len().saturating_sub(MAX_SUBSCRIPTIONS);
        s.subscriptions.drain(..extra);
        key
    })
}

/// Removes a subscription by its endpoint, id or action key; true if one went.
pub fn remove(which: &str) -> Result<bool> {
    update(|s| {
        let before = s.subscriptions.len();
        s.subscriptions.retain(|x| x.endpoint != which && x.id != which && !keys_match(&x.key, which));
        s.subscriptions.len() != before
    })
}

/// The device looked (its viewer is open): clear its badge.
pub fn seen(endpoint: &str) {
    let _ = update(|s| {
        for x in s.subscriptions.iter_mut().filter(|x| x.endpoint == endpoint) {
            x.unread = 0;
        }
    });
}

/// The owner subscription holding this action key.
pub fn owner_by_key(key: &str) -> Option<Subscription> {
    load().subscriptions.into_iter().find(|x| x.owner && keys_match(&x.key, key))
}

fn keys_match(a: &str, b: &str) -> bool {
    !a.is_empty() && crate::control::token_matches(a, b)
}

/// Only real push services: a viewer must not be able to make this host post to any URL.
fn check_endpoint(endpoint: &str) -> Result<()> {
    let host = endpoint
        .strip_prefix("https://")
        .and_then(|rest| rest.split('/').next())
        .context("push endpoint must be https")?;
    const SERVICES: [&str; 6] = [
        "fcm.googleapis.com",
        "android.googleapis.com",
        "push.apple.com",
        "push.services.mozilla.com",
        "notify.windows.com",
        "push.api.chromium.org",
    ];
    if SERVICES.iter().any(|s| host == *s || host.ends_with(&format!(".{s}"))) {
        Ok(())
    } else {
        bail!("unknown push service {host}")
    }
}

fn random_id(bytes: usize) -> String {
    let mut buf = vec![0u8; bytes];
    SystemRandom::new().fill(&mut buf).expect("random");
    B64.encode(buf)
}

// ---- VAPID ----

fn vapid_file() -> PathBuf {
    crate::config_dir().join("vapid.p8")
}

static VAPID: Mutex<Option<std::sync::Arc<EcdsaKeyPair>>> = Mutex::new(None);

fn vapid() -> Result<std::sync::Arc<EcdsaKeyPair>> {
    let mut slot = VAPID.lock().unwrap();
    if let Some(k) = slot.as_ref() {
        return Ok(k.clone());
    }
    let rng = SystemRandom::new();
    let pkcs8 = match std::fs::read(vapid_file()) {
        Ok(b) => b,
        Err(_) => {
            let doc = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &rng).map_err(|_| anyhow::anyhow!("generate a push key"))?;
            crate::settings::write_private(&vapid_file(), doc.as_ref())?;
            doc.as_ref().to_vec()
        }
    };
    let key = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &pkcs8, &rng).map_err(|_| anyhow::anyhow!("read the push key"))?;
    let key = std::sync::Arc::new(key);
    *slot = Some(key.clone());
    Ok(key)
}

/// The public key browsers subscribe with (`applicationServerKey`), base64url.
pub fn public_key() -> Option<String> {
    vapid().ok().map(|k| B64.encode(k.public_key().as_ref()))
}

/// `Authorization: vapid t=<JWT>, k=<public key>` for this endpoint's push service.
fn authorization(endpoint: &str) -> Result<String> {
    let key = vapid()?;
    let origin = endpoint.splitn(4, '/').take(3).collect::<Vec<_>>().join("/");
    let header = B64.encode(br#"{"typ":"JWT","alg":"ES256"}"#);
    let claims = B64.encode(serde_json::to_vec(&json!({
        "aud": origin,
        "exp": crate::access::now() + 12 * 3600,
        "sub": "https://glassdesk.dev",
    }))?);
    let signing_input = format!("{header}.{claims}");
    let sig = key
        .sign(&SystemRandom::new(), signing_input.as_bytes())
        .map_err(|_| anyhow::anyhow!("sign"))?;
    Ok(format!("vapid t={signing_input}.{}, k={}", B64.encode(sig.as_ref()), B64.encode(key.public_key().as_ref())))
}

// ---- Encryption (RFC 8291, aes128gcm) ----

struct Len(usize);

impl hkdf::KeyType for Len {
    fn len(&self) -> usize {
        self.0
    }
}

fn hkdf(salt: &[u8], ikm: &[u8], info: &[u8], len: usize) -> Vec<u8> {
    let prk = hkdf::Salt::new(hkdf::HKDF_SHA256, salt).extract(ikm);
    let mut out = vec![0u8; len];
    prk.expand(&[info], Len(len)).expect("hkdf length").fill(&mut out).expect("hkdf fill");
    out
}

/// Encrypts `plaintext` for a browser: the `aes128gcm` body, one record.
fn encrypt(plaintext: &[u8], ua_public: &[u8], auth: &[u8]) -> Result<Vec<u8>> {
    let rng = SystemRandom::new();
    let private = agreement::EphemeralPrivateKey::generate(&agreement::ECDH_P256, &rng).map_err(|_| anyhow::anyhow!("ecdh key"))?;
    let as_public = private.compute_public_key().map_err(|_| anyhow::anyhow!("ecdh key"))?;
    let mut salt = [0u8; 16];
    rng.fill(&mut salt).map_err(|_| anyhow::anyhow!("random"))?;
    let peer = agreement::UnparsedPublicKey::new(&agreement::ECDH_P256, ua_public);
    let ecdh = agreement::agree_ephemeral(private, &peer, |s| s.to_vec()).map_err(|_| anyhow::anyhow!("ecdh"))?;
    encrypt_with(plaintext, ua_public, auth, &ecdh, as_public.as_ref(), salt)
}

/// The rest of RFC 8291 once the ECDH secret is known.
fn encrypt_with(plaintext: &[u8], ua_public: &[u8], auth: &[u8], ecdh: &[u8], as_public: &[u8], salt: [u8; 16]) -> Result<Vec<u8>> {
    let mut key_info = b"WebPush: info\0".to_vec();
    key_info.extend_from_slice(ua_public);
    key_info.extend_from_slice(as_public);
    let ikm = hkdf(auth, ecdh, &key_info, 32);
    let cek = hkdf(&salt, &ikm, b"Content-Encoding: aes128gcm\0", 16);
    let nonce = hkdf(&salt, &ikm, b"Content-Encoding: nonce\0", 12);

    let mut record = plaintext.to_vec();
    record.push(2); // the last (only) record, no padding
    let key = aead::LessSafeKey::new(aead::UnboundKey::new(&aead::AES_128_GCM, &cek).map_err(|_| anyhow::anyhow!("aes key"))?);
    let nonce = aead::Nonce::try_assume_unique_for_key(&nonce).map_err(|_| anyhow::anyhow!("nonce"))?;
    key.seal_in_place_append_tag(nonce, aead::Aad::empty(), &mut record).map_err(|_| anyhow::anyhow!("encrypt"))?;

    let mut body = Vec::with_capacity(86 + record.len());
    body.extend_from_slice(&salt);
    body.extend_from_slice(&4096u32.to_be_bytes());
    body.push(as_public.len() as u8);
    body.extend_from_slice(as_public);
    body.extend_from_slice(&record);
    Ok(body)
}

// ---- Sending ----

/// One notification.
#[derive(Debug, Clone)]
pub struct Note {
    pub kind: Kind,
    pub title: String,
    pub body: String,
    /// Replaces an earlier notification with the same tag.
    pub tag: String,
    /// For the service worker: what the actions answer (`connect` / `agent`) and its id.
    pub action: Option<(&'static str, u64, &'static str, &'static str)>,
}

/// Sends to every device that wants this kind. Never blocks; failures are logged.
pub fn send(note: Note) {
    let subs: Vec<Subscription> = load().subscriptions.into_iter().filter(|s| s.wants(note.kind)).collect();
    if subs.is_empty() {
        return;
    }
    // Count it against each device's badge first.
    let unread: std::collections::HashMap<String, u32> = update(|s| {
        s.subscriptions
            .iter_mut()
            .filter(|x| subs.iter().any(|y| y.endpoint == x.endpoint))
            .map(|x| {
                x.unread = x.unread.saturating_add(1);
                (x.endpoint.clone(), x.unread)
            })
            .collect()
    })
    .unwrap_or_default();
    std::thread::spawn(move || deliver_all(&note, subs, &unread));
}

/// Delivers to each device in turn; returns how many took it.
fn deliver_all(note: &Note, subs: Vec<Subscription>, unread: &std::collections::HashMap<String, u32>) -> usize {
    let mut ok = 0;
    for sub in subs {
        let badge = unread.get(&sub.endpoint).copied().unwrap_or(sub.unread.max(1));
        match deliver(&sub, &payload(note, &sub, badge)) {
            Ok(true) => ok += 1,
            Ok(false) => {}
            Err(Gone) => {
                tracing::info!("push: {} unsubscribed; forgetting it", sub.device);
                let _ = remove(&sub.endpoint);
            }
        }
    }
    ok
}

/// A test notification to every device (or one, by id), now; how many took it.
pub fn test(id: Option<&str>) -> Result<usize> {
    let subs: Vec<Subscription> = list().into_iter().filter(|s| id.is_none_or(|id| s.id == id)).collect();
    if subs.is_empty() {
        bail!(if id.is_some() { "no such device" } else { "no devices have notifications on" });
    }
    let note = Note {
        kind: Kind::Chat,
        title: "Glass".into(),
        body: "Notifications work on this device.".into(),
        tag: "test".into(),
        action: None,
    };
    Ok(deliver_all(&note, subs, &Default::default()))
}

/// The message: Declarative Web Push (Safari shows it with no service worker code), which
/// other browsers hand to the service worker as is.
fn payload(note: &Note, sub: &Subscription, badge: u32) -> Value {
    let mut data = json!({ "kind": match note.kind { Kind::Chat => "chat", Kind::Ask => "ask", Kind::Activity => "activity" } });
    if let Some((what, id, yes, no)) = note.action {
        data["answer"] = json!({ "what": what, "id": id, "yes": yes, "no": no });
    }
    json!({
        "web_push": 8030,
        "notification": {
            "title": note.title,
            "body": note.body.chars().take(400).collect::<String>(),
            "navigate": if sub.url.is_empty() { "/".to_string() } else { sub.url.clone() },
            "tag": note.tag,
            "silent": false,
            "data": data,
        },
        "app_badge": badge,
    })
}

struct Gone;

/// Ok(true): delivered; Ok(false): failed for now; Err(Gone): the subscription is dead.
fn deliver(sub: &Subscription, payload: &Value) -> Result<bool, Gone> {
    // Tests: record what would be sent instead of calling a real push service.
    if crate::testmode::enabled() {
        crate::testmode::record("push", json!({ "device": sub.device, "endpoint": sub.endpoint, "payload": payload }));
        return Ok(true);
    }
    let result = (|| -> Result<u16> {
        let ua_public = B64.decode(sub.p256dh.trim_end_matches('='))?;
        let auth = B64.decode(sub.auth.trim_end_matches('='))?;
        let body = encrypt(&serde_json::to_vec(payload)?, &ua_public, &auth)?;
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(Duration::from_secs(15)))
            .http_status_as_error(false)
            .build()
            .new_agent();
        let response = agent
            .post(&sub.endpoint)
            .header("Authorization", authorization(&sub.endpoint)?)
            .header("Content-Encoding", "aes128gcm")
            .header("Content-Type", "application/octet-stream")
            .header("TTL", TTL_SECS.to_string())
            .header("Urgency", "high")
            .send(&body[..])?;
        Ok(response.status().as_u16())
    })();
    match result {
        Ok(200..=299) => Ok(true),
        // The browser unsubscribed, or the subscription expired.
        Ok(404 | 410) => Err(Gone),
        Ok(code) => {
            tracing::warn!("push to {}: HTTP {code}", sub.device);
            Ok(false)
        }
        Err(e) => {
            tracing::warn!("push to {}: {e:#}", sub.device);
            Ok(false)
        }
    }
}

// ---- Events ----

pub fn chat(from: &str, text: &str) {
    send(Note { kind: Kind::Chat, title: from.into(), body: text.into(), tag: "chat".into(), action: None });
}

pub fn connect_request(id: u64, who: &str) {
    send(Note {
        kind: Kind::Ask,
        title: "Someone wants to connect".into(),
        body: who.into(),
        tag: format!("connect-{id}"),
        action: Some(("connect", id, "Allow", "Deny")),
    });
}

pub fn agent_ask(id: u64, kind: &str, text: &str) {
    let title = match kind {
        "help" => "An agent needs your help",
        "consent" => "An agent asks to use this computer",
        _ => "An agent asks before it acts",
    };
    send(Note {
        kind: Kind::Ask,
        title: title.into(),
        // A consent's text ends with its request id and an instruction; the notification says it once.
        body: if kind == "consent" {
            let text = text.rsplit_once(" [").map_or(text, |(t, _)| t);
            let text = text.trim_end_matches(" Tap the number if its sign-in page shows it.");
            format!("{text} Open Glass and tap the number its sign-in page shows.")
        } else {
            text.into()
        },
        tag: format!("agent-{id}"),
        // A consent needs the number from the sign-in page, so it's answered in Glass.
        action: (kind == "confirm").then_some(("agent", id, "Allow", "Don't")),
    });
}

pub fn activity(title: &str, body: &str, tag: &str) {
    send(Note { kind: Kind::Activity, title: title.into(), body: body.into(), tag: tag.into(), action: None });
}

// ---- HTTP: a notification's Allow / Deny ----

#[derive(Deserialize)]
struct Act {
    key: String,
    what: String,
    id: u64,
    yes: bool,
}

pub fn routes() -> axum::Router {
    axum::Router::new().route("/push/act", axum::routing::post(act))
}

async fn act(axum::Json(a): axum::Json<Act>) -> (axum::http::StatusCode, axum::Json<Value>) {
    use axum::http::StatusCode;
    let Some(sub) = owner_by_key(&a.key) else {
        tokio::time::sleep(Duration::from_millis(300)).await;
        return (StatusCode::FORBIDDEN, axum::Json(json!({ "error": "unknown device" })));
    };
    let ok = match a.what.as_str() {
        "connect" => crate::approval::decide(a.id, a.yes),
        "agent" => {
            crate::computer::answer(a.id, crate::computer::Answer { ok: a.yes, note: None });
            true
        }
        _ => false,
    };
    tracing::info!("push: {} answered {} {} from a notification", sub.device, a.what, if a.yes { "yes" } else { "no" });
    (StatusCode::OK, axum::Json(json!({ "ok": ok })))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// RFC 8291 appendix A.
    #[test]
    fn encrypts_like_the_rfc_example() {
        let d = |s: &str| B64.decode(s).unwrap();
        let ua_public = d("BCVxsr7N_eNgVRqvHtD0zTZsEc6-VV-JvLexhqUzORcxaOzi6-AYWXvTBHm4bjyPjs7Vd8pZGH6SRpkNtoIAiw4");
        let auth = d("BTBZMqHH6r4Tts7J_aSIgg");
        let as_public = d("BP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27mlmlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A8");
        let ecdh = d("kyrL1jIIOHEzg3sM2ZWRHDRB62YACZhhSlknJ672kSs");
        let salt: [u8; 16] = d("DGv6ra1nlYgDCS1FRnbzlw").try_into().unwrap();
        let body = encrypt_with(b"When I grow up, I want to be a watermelon", &ua_public, &auth, &ecdh, &as_public, salt).unwrap();
        assert_eq!(
            B64.encode(body),
            "DGv6ra1nlYgDCS1FRnbzlwAAEABBBP4z9KsN6nGRTbVYI_c7VJSPQTBtkgcy27mlmlMoZIIgDll6e3vCYLocInmYWAmS6TlzAC8wEqKK6PBru3jl7A_yl95bQpu6cVPTpK4Mqgkf1CXztLVBSt2Ks3oZwbuwXPXLWyouBWLVWGNWQexSgSxsj_Qulcy4a-fN"
        );
    }

    #[test]
    fn only_real_push_services() {
        assert!(check_endpoint("https://fcm.googleapis.com/fcm/send/abc").is_ok());
        assert!(check_endpoint("https://web.push.apple.com/QO").is_ok());
        assert!(check_endpoint("https://updates.push.services.mozilla.com/wpush/v2/x").is_ok());
        assert!(check_endpoint("https://evil.example/fcm.googleapis.com").is_err());
        assert!(check_endpoint("http://fcm.googleapis.com/x").is_err());
        assert!(check_endpoint("https://fcm.googleapis.com.evil.example/x").is_err());
        assert!(check_endpoint("https://localhost:8080/").is_err());
    }
}
