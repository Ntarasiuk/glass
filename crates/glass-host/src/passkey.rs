//! Passkeys for guest passes (WebAuthn, ES256). A link made with `--passkey` makes the
//! device that opens it register a passkey; from then on the pass alone isn't enough, the
//! browser must also sign a fresh challenge with that passkey.
//!
//! TODO(passkey agent): verification. Interface is fixed.

use serde::{Deserialize, Serialize};

/// Sent to the browser: create a passkey, or sign in with one.
#[derive(Debug, Clone, Serialize)]
pub struct Challenge {
    /// "create" or "get".
    pub mode: &'static str,
    /// Random bytes, base64url.
    pub challenge: String,
    /// Name shown by the browser's passkey sheet.
    pub user: String,
    /// For "get": the credential to use, base64url.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub credential: Option<String>,
}

/// The browser's answer; binary fields are base64url.
#[derive(Debug, Clone, Deserialize)]
pub struct Response {
    pub mode: String,
    /// Credential id.
    pub id: String,
    /// clientDataJSON.
    pub client_data: String,
    /// For "get".
    #[serde(default)]
    pub authenticator_data: Option<String>,
    #[serde(default)]
    pub signature: Option<String>,
    /// For "create": the public key as SubjectPublicKeyInfo DER (getPublicKey()).
    #[serde(default)]
    pub public_key: Option<String>,
    /// COSE algorithm (getPublicKeyAlgorithm()); only -7 (ES256) is accepted.
    #[serde(default)]
    pub algorithm: Option<i64>,
}

/// Has the device that just opened a link register a passkey, and binds it to `pass`.
/// False if the browser declined or the answer didn't check out.
pub async fn register(
    send: &mut wtransport::SendStream,
    recv: &mut wtransport::RecvStream,
    access_file: &std::path::Path,
    pass: &str,
    user: &str,
) -> anyhow::Result<bool> {
    let _ = (send, recv, access_file, pass, user);
    Ok(false)
}

/// Asks the browser to sign a fresh challenge with the passkey bound to `pass`.
pub async fn verify(
    send: &mut wtransport::SendStream,
    recv: &mut wtransport::RecvStream,
    access_file: &std::path::Path,
    pass: &str,
    key: &crate::access::Passkey,
) -> anyhow::Result<bool> {
    let _ = (send, recv, access_file, pass, key);
    Ok(false)
}
