//! "Trust this computer": the page (web/trust.html) and files a viewer needs to install
//! the local CA (`glass tls init`), so the pages load over HTTPS. A browser can't install
//! a CA from script, so the page offers the certificate (PEM, and a .mobileconfig for
//! iOS and macOS), its fingerprint to compare, instructions per system, and a check that
//! switches to HTTPS once the browser trusts it.
//!
//! Served on the web port with the rest, and with --trust-port also over plain HTTP:
//! a viewer that doesn't trust the CA yet gets a certificate warning on HTTPS. Over a
//! tailnet that plain HTTP is still encrypted (WireGuard); elsewhere, compare the
//! fingerprint with `glass tls status` on this computer.

use std::path::PathBuf;

use axum::extract::Request;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use axum::{Json, Router};
use base64::Engine;
use tower_http::services::ServeFile;

/// Routes for the main web app: /trust, /trust/info.json, /trust/ca.crt,
/// /trust/ca.mobileconfig. `https_port` is where the pages are served over HTTPS.
pub fn routes(web_dir: PathBuf, https_port: u16) -> Router {
    Router::new()
        .route_service("/trust", ServeFile::new(web_dir.join("trust.html")))
        .route("/trust/info.json", get(move |headers: HeaderMap| info(headers, https_port)))
        .route("/trust/ca.crt", get(ca_pem))
        .route("/trust/ca.mobileconfig", get(mobileconfig))
}

/// The plain-HTTP listener (--trust-port): the trust page and its files, nothing else.
pub fn http_app(web_dir: PathBuf, https_port: u16) -> Router {
    let icon = web_dir.join("icon.svg");
    routes(web_dir, https_port)
        .route_service("/icon.svg", ServeFile::new(icon))
        .fallback(|| async { Redirect::temporary("/trust") })
        .layer(tower_http::set_header::SetResponseHeaderLayer::overriding(
            header::CACHE_CONTROL,
            HeaderValue::from_static("no-store"),
        ))
}

/// The host part of a Host header ("name:port", "[v6]:port").
fn host_of(headers: &HeaderMap) -> Option<String> {
    let host = headers.get(header::HOST)?.to_str().ok()?;
    let name = if let Some(rest) = host.strip_prefix('[') {
        format!("[{}]", rest.split(']').next()?)
    } else {
        host.rsplit_once(':').map_or(host, |(h, _)| h).to_string()
    };
    (!name.is_empty()).then_some(name)
}

async fn info(headers: HeaderMap, https_port: u16) -> Json<serde_json::Value> {
    let ca = crate::local_ca::ca_cert();
    let details = crate::local_ca::read_cert(&crate::local_ca::files().ca_cert).ok();
    let https = host_of(&headers).map(|h| match https_port {
        443 => format!("https://{h}/"),
        p => format!("https://{h}:{p}/"),
    });
    Json(serde_json::json!({
        "available": ca.is_some(),
        "fingerprint": ca.as_ref().map(|(_, _, sha)| crate::local_ca::fingerprint(sha)),
        "name": details.as_ref().map(|d| d.subject.trim_start_matches("CN=").to_string()),
        "onlyFor": details.as_ref().and_then(|d| d.permitted.as_ref()).map(|p| p.describe()),
        "expires": details.as_ref().map(|d| d.not_after.date().to_string()),
        "https": https,
    }))
}

fn not_found() -> Response {
    (StatusCode::NOT_FOUND, "This computer has no local CA (glass tls init).").into_response()
}

async fn ca_pem(_: Request) -> Response {
    let Some((pem, _, _)) = crate::local_ca::ca_cert() else {
        return not_found();
    };
    (
        [
            (header::CONTENT_TYPE, "application/x-x509-ca-cert"),
            (header::CONTENT_DISPOSITION, "attachment; filename=\"glass-ca.crt\""),
        ],
        pem,
    )
        .into_response()
}

/// A UUID-shaped id from the certificate's hash, so reinstalling replaces the profile.
fn uuid(sha: &[u8; 32], salt: u8) -> String {
    let b: Vec<u8> = sha.iter().map(|x| x ^ salt).take(16).collect();
    let hex: String = b.iter().map(|x| format!("{x:02X}")).collect();
    format!("{}-{}-{}-{}-{}", &hex[..8], &hex[8..12], &hex[12..16], &hex[16..20], &hex[20..32])
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;")
}

/// An Apple configuration profile with the CA (iOS, iPadOS, macOS).
pub fn mobileconfig_for(der: &[u8], sha: &[u8; 32], name: &str) -> String {
    let data = base64::engine::general_purpose::STANDARD.encode(der);
    let name = xml_escape(name);
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>PayloadContent</key>
  <array>
    <dict>
      <key>PayloadCertificateFileName</key><string>glass-ca.cer</string>
      <key>PayloadContent</key><data>{data}</data>
      <key>PayloadDescription</key><string>Lets this device open Glass on this computer over HTTPS.</string>
      <key>PayloadDisplayName</key><string>{name}</string>
      <key>PayloadIdentifier</key><string>app.glass.ca.{id}</string>
      <key>PayloadType</key><string>com.apple.security.root</string>
      <key>PayloadUUID</key><string>{cert_uuid}</string>
      <key>PayloadVersion</key><integer>1</integer>
    </dict>
  </array>
  <key>PayloadDisplayName</key><string>{name}</string>
  <key>PayloadIdentifier</key><string>app.glass.trust.{id}</string>
  <key>PayloadRemovalDisallowed</key><false/>
  <key>PayloadType</key><string>Configuration</string>
  <key>PayloadUUID</key><string>{profile_uuid}</string>
  <key>PayloadVersion</key><integer>1</integer>
</dict>
</plist>
"#,
        id = &uuid(sha, 0)[..8],
        cert_uuid = uuid(sha, 0x5a),
        profile_uuid = uuid(sha, 0xa5),
    )
}

async fn mobileconfig(_: Request) -> Response {
    let Some((_, der, sha)) = crate::local_ca::ca_cert() else {
        return not_found();
    };
    let name = crate::local_ca::read_cert(&crate::local_ca::files().ca_cert)
        .map(|d| d.subject.trim_start_matches("CN=").to_string())
        .unwrap_or_else(|_| "Glass local CA".into());
    (
        [
            (header::CONTENT_TYPE, "application/x-apple-aspen-config"),
            (header::CONTENT_DISPOSITION, "attachment; filename=\"glass-ca.mobileconfig\""),
        ],
        mobileconfig_for(&der, &sha, &name),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_header_without_port() {
        let mut h = HeaderMap::new();
        for (value, want) in [
            ("pc.tail.example:9090", "pc.tail.example"),
            ("pc", "pc"),
            ("100.64.0.9:80", "100.64.0.9"),
            ("[fd7a::1]:9090", "[fd7a::1]"),
        ] {
            h.insert(header::HOST, HeaderValue::from_static(value));
            assert_eq!(host_of(&h).as_deref(), Some(want), "{value}");
        }
    }

    #[test]
    fn profile_carries_the_certificate() {
        let sha = [7u8; 32];
        let profile = mobileconfig_for(b"DER", &sha, "Glass <pc> CA");
        assert!(profile.contains("<data>REVS</data>"));
        assert!(profile.contains("com.apple.security.root"));
        assert!(profile.contains("Glass &lt;pc&gt; CA"));
        assert_ne!(uuid(&sha, 0x5a), uuid(&sha, 0xa5));
    }
}
