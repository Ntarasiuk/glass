//! Directory sign-in (LDAP, e.g. Active Directory, OpenLDAP, FreeIPA): people type their
//! directory username and password on the sign-in screen; the host binds as them, reads
//! their email and groups, and the access rules (acl.rs) decide what they get: a guest
//! pass, as single sign-on mints.
//!
//! Either a DN template (`ldap-user-dn uid={user},ou=people,…`, one bind), or a search
//! under `ldap-base-dn` (as `ldap-bind-dn`, or anonymously) followed by a bind as the
//! entry found. `ldaps://` is TLS; `ldap://` upgrades with StartTLS, except to localhost.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use axum::extract::ConnectInfo;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Deserialize;
use serde_json::json;
use tracing::{info, warn};

const DEFAULT_FILTER: &str = "(|(uid={user})(mail={user}))";
const DEFAULT_GROUP_ATTR: &str = "memberOf";
const TIMEOUT: Duration = Duration::from_secs(8);
/// Failed sign-ins allowed per address within `WINDOW`.
const MAX_FAILURES: u32 = 10;
const WINDOW: Duration = Duration::from_secs(600);

#[derive(Clone)]
struct Config {
    url: String,
    user_dn: Option<String>,
    base_dn: Option<String>,
    filter: String,
    bind_dn: Option<String>,
    bind_password: Option<String>,
    group_attr: String,
}

fn config() -> Option<Config> {
    let s = crate::settings::load();
    let url = s.ldap_url?;
    if s.ldap_user_dn.is_none() && s.ldap_base_dn.is_none() {
        return None;
    }
    Some(Config {
        url,
        user_dn: s.ldap_user_dn,
        base_dn: s.ldap_base_dn,
        filter: s.ldap_user_filter.unwrap_or_else(|| DEFAULT_FILTER.into()),
        bind_dn: s.ldap_bind_dn,
        bind_password: crate::settings::ldap_bind_password(),
        group_attr: s.ldap_group_attr.unwrap_or_else(|| DEFAULT_GROUP_ATTR.into()),
    })
}

/// Whether the sign-in screen should offer directory sign-in.
pub fn enabled() -> bool {
    config().is_some()
}

pub fn routes() -> axum::Router {
    axum::Router::new().route("/sso/ldap", axum::routing::post(sign_in_route))
}

// ---- Pure parts (unit tested) ----

/// Escapes a value for use in a DN (RFC 4514 §2.4).
pub fn escape_dn_value(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    let last = v.chars().count().saturating_sub(1);
    for (i, c) in v.chars().enumerate() {
        match c {
            ',' | '+' | '"' | '\\' | '<' | '>' | ';' | '=' => {
                out.push('\\');
                out.push(c);
            }
            '#' if i == 0 => out.push_str("\\#"),
            ' ' if i == 0 || i == last => out.push_str("\\ "),
            '\0' => out.push_str("\\00"),
            c => out.push(c),
        }
    }
    out
}

/// Escapes a value for use in a search filter (RFC 4515 §3).
pub fn escape_filter(v: &str) -> String {
    let mut out = String::with_capacity(v.len());
    for b in v.bytes() {
        match b {
            b'*' | b'(' | b')' | b'\\' | 0 => out.push_str(&format!("\\{b:02x}")),
            _ => out.push(b as char),
        }
    }
    out
}

pub fn user_dn(template: &str, user: &str) -> String {
    template.replace("{user}", &escape_dn_value(user))
}

pub fn user_filter(template: &str, user: &str) -> String {
    template.replace("{user}", &escape_filter(user))
}

/// A group's name from its DN: the first RDN's value (`cn=eng,ou=groups,…` → `eng`).
pub fn group_name(dn: &str) -> String {
    let mut first = String::new();
    let mut escaped = false;
    for c in dn.chars() {
        if escaped {
            first.push(c);
            escaped = false;
        } else if c == '\\' {
            escaped = true;
        } else if c == ',' {
            break;
        } else {
            first.push(c);
        }
    }
    match first.split_once('=') {
        Some((_, v)) => v.trim().to_string(),
        None => first.trim().to_string(),
    }
}

/// Plain `ldap://` without StartTLS only to this machine (or in test mode).
fn plain_ok(url: &str) -> bool {
    let host = url
        .trim_start_matches("ldap://")
        .split(['/', ':'])
        .next()
        .unwrap_or("");
    crate::testmode::enabled() || matches!(host, "localhost" | "127.0.0.1" | "[::1]")
}

// ---- The directory ----

/// Who signed in: their email (or the name typed, without one) and group names.
#[derive(Debug)]
pub struct Person {
    pub email: String,
    pub groups: Vec<String>,
}

/// Checks `user`'s password; `Ok(None)` for wrong credentials.
pub async fn authenticate(user: &str, password: &str) -> Result<Option<Person>> {
    let cfg = config().context("directory sign-in isn't set up")?;
    // An empty password is an "unauthenticated bind", which servers accept: never.
    if user.trim().is_empty() || password.is_empty() || user.len() > 256 || password.len() > 1024 {
        return Ok(None);
    }
    let user = user.trim();
    tokio::time::timeout(TIMEOUT * 2, directory(&cfg, user, password))
        .await
        .context("the directory didn't answer in time")?
}

async fn directory(cfg: &Config, user: &str, password: &str) -> Result<Option<Person>> {
    use ldap3::{LdapConnAsync, LdapConnSettings, Scope, SearchEntry};
    let starttls = cfg.url.starts_with("ldap://") && !plain_ok(&cfg.url);
    let settings = LdapConnSettings::new()
        .set_conn_timeout(TIMEOUT)
        .set_starttls(starttls);
    let (conn, mut ldap) = LdapConnAsync::with_settings(settings, &cfg.url)
        .await
        .context("connect to the directory")?;
    ldap3::drive!(conn);
    ldap.with_timeout(TIMEOUT);

    let dn = match &cfg.user_dn {
        Some(t) => user_dn(t, user),
        None => {
            let base = cfg.base_dn.as_deref().context("no ldap-base-dn")?;
            if let (Some(dn), Some(pw)) = (&cfg.bind_dn, &cfg.bind_password) {
                ldap.simple_bind(dn, pw)
                    .await?
                    .success()
                    .context("the search account (ldap-bind-dn) was refused")?;
            }
            let filter = user_filter(&cfg.filter, user);
            let (entries, _) = ldap
                .search(base, Scope::Subtree, &filter, vec!["1.1"])
                .await?
                .success()
                .context("search for the user")?;
            match entries.as_slice() {
                [one] => SearchEntry::construct(one.clone()).dn,
                [] => return Ok(None),
                _ => {
                    warn!("ldap: {} entries match {filter}; refusing", entries.len());
                    return Ok(None);
                }
            }
        }
    };
    let bound = ldap.simple_bind(&dn, password).await?;
    // 49: invalidCredentials.
    if bound.rc == 49 {
        return Ok(None);
    }
    bound.success().context("bind")?;
    let attrs = vec!["mail", cfg.group_attr.as_str()];
    let (entries, _) = ldap
        .search(&dn, Scope::Base, "(objectClass=*)", attrs)
        .await?
        .success()
        .context("read the user's entry")?;
    let entry = entries
        .into_iter()
        .next()
        .map(SearchEntry::construct)
        .context("the user's entry is gone")?;
    let _ = ldap.unbind().await;
    let pick = |name: &str| {
        entry
            .attrs
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.clone())
            .unwrap_or_default()
    };
    let email = pick("mail")
        .into_iter()
        .next()
        .unwrap_or_else(|| user.to_string())
        .to_ascii_lowercase();
    let groups = pick(&cfg.group_attr).iter().map(|g| group_name(g)).collect();
    Ok(Some(Person { email, groups }))
}

// ---- POST /sso/ldap {user, password} → {pass, expires} ----

static FAILURES: Mutex<Option<HashMap<IpAddr, (u32, Instant)>>> = Mutex::new(None);

fn blocked(ip: IpAddr) -> bool {
    let mut f = FAILURES.lock().unwrap();
    let f = f.get_or_insert_with(HashMap::new);
    f.retain(|_, (_, since)| since.elapsed() < WINDOW);
    f.get(&ip).is_some_and(|(n, _)| *n >= MAX_FAILURES)
}

fn failed(ip: IpAddr) {
    let mut f = FAILURES.lock().unwrap();
    let e = f
        .get_or_insert_with(HashMap::new)
        .entry(ip)
        .or_insert((0, Instant::now()));
    e.0 += 1;
}

#[derive(Deserialize)]
struct SignIn {
    user: String,
    password: String,
}

fn refuse(status: StatusCode, msg: &str) -> Response {
    (status, axum::Json(json!({ "error": msg }))).into_response()
}

async fn sign_in_route(
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    axum::Json(body): axum::Json<SignIn>,
) -> Response {
    let ip = peer.ip().to_canonical();
    if !enabled() {
        return refuse(StatusCode::NOT_FOUND, "Directory sign-in isn't set up on this computer.");
    }
    if blocked(ip) {
        return refuse(StatusCode::TOO_MANY_REQUESTS, "Too many attempts. Try again later.");
    }
    let refused = |detail: String| {
        crate::connlog::append(&crate::connlog::Entry {
            start: crate::access::now(),
            secs: 0,
            device: body.user.chars().take(64).collect(),
            how: "refused".into(),
            remote: false,
            peer: ip.to_string(),
            detail,
        });
    };
    let person = match authenticate(&body.user, &body.password).await {
        Ok(Some(p)) => p,
        Ok(None) => {
            failed(ip);
            refused("ldap: wrong username or password".into());
            // Slows guessing down further.
            tokio::time::sleep(Duration::from_millis(500)).await;
            return refuse(StatusCode::FORBIDDEN, "That username or password didn't work.");
        }
        Err(e) => {
            warn!("ldap: {e:#}");
            return refuse(StatusCode::BAD_GATEWAY, "Couldn't reach the directory. Try again.");
        }
    };
    match grant(&person) {
        Ok(Some(g)) => {
            info!("ldap: {} signed in ({})", person.email, g.scope.describe());
            crate::connlog::append(&crate::connlog::Entry {
                start: crate::access::now(),
                secs: 0,
                device: person.email.clone(),
                how: "ldap".into(),
                remote: false,
                peer: ip.to_string(),
                detail: format!("ldap {}", person.email),
            });
            axum::Json(json!({ "pass": g.pass, "expires": g.expires })).into_response()
        }
        Ok(None) => {
            refused(format!("ldap {}: no access rule", person.email));
            refuse(StatusCode::FORBIDDEN, "That account isn't allowed to use this computer.")
        }
        Err(e) => {
            warn!("ldap: {e:#}");
            refuse(StatusCode::INTERNAL_SERVER_ERROR, "Sign-in didn't work. Try again.")
        }
    }
}

/// A guest pass for a directory account the rules allow; `None` if they don't (or SCIM
/// deactivated it).
pub fn grant(person: &Person) -> Result<Option<crate::access::Granted>> {
    let mut groups = person.groups.clone();
    match crate::scim::account(&person.email) {
        crate::scim::Account::Inactive => return Ok(None),
        crate::scim::Account::Active(g) => groups.extend(g),
        crate::scim::Account::Unknown => {}
    }
    let Some((scope, duration)) = crate::acl::decide(&crate::acl::effective(), &person.email, &groups) else {
        return Ok(None);
    };
    let shared = crate::server::shared().context("host not ready")?;
    let file = crate::access::file_for(&shared.token_file);
    Ok(Some(crate::access::grant_pass(&file, duration, scope, &person.email)?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dn_values_are_escaped() {
        assert_eq!(escape_dn_value("ana"), "ana");
        assert_eq!(escape_dn_value("a,b+c"), "a\\,b\\+c");
        assert_eq!(escape_dn_value("#lead"), "\\#lead");
        assert_eq!(escape_dn_value(" pad "), "\\ pad\\ ");
        assert_eq!(escape_dn_value("x=y;z\\"), "x\\=y\\;z\\\\");
        assert_eq!(
            user_dn("uid={user},ou=people,dc=example,dc=com", "evil,ou=admins"),
            "uid=evil\\,ou\\=admins,ou=people,dc=example,dc=com"
        );
    }

    #[test]
    fn filter_values_are_escaped() {
        assert_eq!(escape_filter("ana"), "ana");
        assert_eq!(escape_filter("*)(uid=*"), "\\2a\\29\\28uid=\\2a");
        assert_eq!(user_filter(DEFAULT_FILTER, "a*"), "(|(uid=a\\2a)(mail=a\\2a))");
    }

    #[test]
    fn group_names_from_dns() {
        assert_eq!(group_name("cn=eng,ou=groups,dc=example,dc=com"), "eng");
        assert_eq!(group_name("CN=Domain Users,CN=Users,DC=corp"), "Domain Users");
        assert_eq!(group_name("cn=a\\,b,ou=x"), "a,b");
        assert_eq!(group_name("eng"), "eng");
    }

    #[test]
    fn plain_ldap_only_to_this_machine() {
        assert!(plain_ok("ldap://localhost:389"));
        assert!(plain_ok("ldap://127.0.0.1"));
        assert!(!plain_ok("ldap://dir.example.com"));
    }
}
