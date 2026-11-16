//! Access rules: who may sign in with a directory account (single sign-on, LDAP) and what
//! they may do. An ordered list in `acl.json`; the first rule that matches decides, and
//! no match means no access. A rule matches an email (`ana@example.com`), a domain
//! (`@example.com`), a directory group (`group:eng`, from SCIM or LDAP) or anyone (`*`).
//!
//! With no `acl.json`, the older settings apply as rules: each `oidc-allow` entry, with
//! `oidc-scope`.

use std::path::PathBuf;
use std::time::Duration;

use anyhow::Result;
use axum::extract::ConnectInfo;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::{Deserialize, Serialize};

use crate::access::Scope;

/// How long a sign-in lasts when a rule doesn't say.
pub const DEFAULT_GRANT: Duration = Duration::from_secs(24 * 3600);

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Rule {
    #[serde(rename = "match")]
    pub matcher: String,
    #[serde(default)]
    pub scope: Scope,
    /// Seconds a sign-in lasts (default a day).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_secs: Option<u64>,
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct Rules {
    #[serde(default)]
    rules: Vec<Rule>,
}

pub fn path() -> PathBuf {
    crate::config_dir().join("acl.json")
}

/// The rules saved in `acl.json` (empty when there's none).
pub fn load() -> Vec<Rule> {
    std::fs::read(path())
        .ok()
        .and_then(|b| serde_json::from_slice::<Rules>(&b).ok())
        .map(|f| f.rules)
        .unwrap_or_default()
}

pub fn save(rules: &[Rule]) -> Result<()> {
    for r in rules {
        validate(&r.matcher)?;
    }
    let p = path();
    if let Some(dir) = p.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = p.with_extension("tmp");
    let body = Rules {
        rules: rules.to_vec(),
    };
    std::fs::write(&tmp, serde_json::to_vec_pretty(&body)?)?;
    std::fs::rename(&tmp, &p)?;
    Ok(())
}

/// The rules in force: `acl.json`, or the implicit ones from the SSO settings.
pub fn effective() -> Vec<Rule> {
    let rules = load();
    if !rules.is_empty() {
        return rules;
    }
    let s = crate::settings::load();
    let scope = Scope {
        view_only: s.oidc_view_only,
        ..Scope::default()
    };
    s.oidc_allow
        .into_iter()
        .map(|matcher| Rule {
            matcher,
            scope,
            duration_secs: None,
        })
        .collect()
}

/// Checks a matcher's form: `*`, `@domain`, `group:name` or an email.
pub fn validate(matcher: &str) -> Result<()> {
    let m = matcher.trim();
    let domain_ok = |d: &str| d.contains('.') && !d.contains('@') && !d.contains(' ');
    let ok = m == "*"
        || m.strip_prefix('@').is_some_and(domain_ok)
        || m.strip_prefix("group:")
            .is_some_and(|g| !g.trim().is_empty() && g.len() <= 256)
        || (!m.contains(' ')
            && m.split_once('@')
                .is_some_and(|(u, d)| !u.is_empty() && domain_ok(d)));
    anyhow::ensure!(ok, "{m} isn't an email, @domain, group:name or *");
    Ok(())
}

fn matches(matcher: &str, email: &str, groups: &[String]) -> bool {
    let m = matcher.trim().to_ascii_lowercase();
    let email = email.to_ascii_lowercase();
    if m == "*" {
        return true;
    }
    if let Some(group) = m.strip_prefix("group:") {
        return groups.iter().any(|g| g.to_ascii_lowercase() == group.trim());
    }
    if let Some(domain) = m.strip_prefix('@') {
        return email.rsplit_once('@').is_some_and(|(_, d)| d == domain);
    }
    m == email
}

/// What `email` (in `groups`) may do, and for how long; `None`: no access.
pub fn decide(rules: &[Rule], email: &str, groups: &[String]) -> Option<(Scope, Duration)> {
    rules
        .iter()
        .find(|r| matches(&r.matcher, email, groups))
        .map(|r| {
            let d = r.duration_secs.map_or(DEFAULT_GRANT, Duration::from_secs);
            (
                r.scope,
                d.clamp(crate::access::MIN_GRANT, crate::access::MAX_GRANT),
            )
        })
}

/// What `email` may do, as the directory and the rules say together: nothing if it's
/// deactivated or deleted there, and with its groups there added to `groups`.
pub fn allowed(dir: &crate::scim::Directory, rules: &[Rule], email: &str, groups: &[String]) -> Option<(Scope, Duration)> {
    let mut groups = groups.to_vec();
    match dir.account(email) {
        crate::scim::Account::Inactive => return None,
        crate::scim::Account::Active(g) => groups.extend(g),
        crate::scim::Account::Unknown => {}
    }
    decide(rules, email, &groups)
}

fn tailnet_scope_in(dir: &crate::scim::Directory, rules: &[Rule], members: bool, login: &str) -> Option<Scope> {
    if matches!(dir.account(login), crate::scim::Account::Inactive) {
        return None;
    }
    allowed(dir, rules, login, &[]).map(|(scope, _)| scope).or_else(|| members.then(Scope::default))
}

/// What someone else on the tailnet (`login`) may do: the access rules' first match, else a
/// guest's default if the owner lets the whole tailnet in; `None`: they need the token. A
/// session holding this is checked again while it lasts.
pub fn tailnet_guest_scope(login: &str) -> Option<Scope> {
    tailnet_scope_in(&crate::scim::load(), &effective(), crate::settings::load().tailnet_members, login)
}

/// One line for `glass acl`.
pub fn describe(i: usize, r: &Rule) -> String {
    let d = r.duration_secs.map_or(DEFAULT_GRANT, Duration::from_secs);
    format!(
        "{i:>3}  {}  {}  for {}",
        r.matcher,
        r.scope.describe(),
        crate::access::describe(d)
    )
}

// ---- The console's API: GET/PUT /admin/api/acl (owner only) ----

pub fn routes() -> axum::Router {
    axum::Router::new().route("/admin/api/acl", axum::routing::get(get_rules).put(put_rules))
}

async fn get_rules(
    ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
) -> Response {
    if !crate::forward::is_owner(peer.ip().to_canonical(), &headers).await {
        return StatusCode::FORBIDDEN.into_response();
    }
    // What's saved, or (flagged `implicit`) the rules the SSO settings amount to.
    let saved = load();
    let implicit = saved.is_empty();
    let rules = if implicit { effective() } else { saved };
    axum::Json(serde_json::json!({ "rules": rules, "implicit": implicit })).into_response()
}

async fn put_rules(
    ConnectInfo(peer): ConnectInfo<std::net::SocketAddr>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    if !crate::forward::is_owner(peer.ip().to_canonical(), &headers).await {
        return StatusCode::FORBIDDEN.into_response();
    }
    if let Some(r) = crate::origin::require_json(&headers) {
        return r;
    }
    let rules = match serde_json::from_slice::<Rules>(&body) {
        Ok(r) => r.rules,
        Err(e) => return (StatusCode::BAD_REQUEST, format!("{e}")).into_response(),
    };
    match save(&rules) {
        Ok(()) => axum::Json(serde_json::json!({ "rules": rules })).into_response(),
        Err(e) => (StatusCode::BAD_REQUEST, format!("{e:#}")).into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rule(m: &str, view_only: bool) -> Rule {
        Rule {
            matcher: m.into(),
            scope: Scope {
                view_only,
                ..Scope::default()
            },
            duration_secs: None,
        }
    }

    #[test]
    fn first_match_wins_and_no_match_is_no_access() {
        let rules = vec![
            rule("boss@example.com", false),
            rule("group:Eng", true),
            rule("@example.com", false),
        ];
        let none: Vec<String> = vec![];
        let eng = vec!["eng".to_string()];
        assert!(!decide(&rules, "BOSS@example.com", &eng).unwrap().0.view_only);
        assert!(decide(&rules, "ana@corp.dev", &eng).unwrap().0.view_only, "by group");
        assert!(!decide(&rules, "ana@example.com", &none).unwrap().0.view_only);
        assert!(decide(&rules, "ana@evil-example.com", &none).is_none());
        assert!(decide(&[], "ana@example.com", &none).is_none());
        assert!(decide(&[rule("*", true)], "x@y.z", &none).is_some());
    }

    #[test]
    fn durations_are_clamped() {
        let mut r = rule("*", false);
        r.duration_secs = Some(1);
        assert_eq!(
            decide(std::slice::from_ref(&r), "a@b.c", &[]).unwrap().1,
            crate::access::MIN_GRANT
        );
        r.duration_secs = Some(8 * 3600);
        assert_eq!(
            decide(&[r], "a@b.c", &[]).unwrap().1,
            Duration::from_secs(8 * 3600)
        );
    }

    #[test]
    fn matchers_are_validated() {
        for ok in ["*", "@example.com", "group:eng", "ana@example.com"] {
            assert!(validate(ok).is_ok(), "{ok}");
        }
        for bad in ["", "example.com", "@nodot", "group:", "a@@b.c", "ana @x.com"] {
            assert!(validate(bad).is_err(), "{bad}");
        }
    }

    fn directory(user: &str, active: bool, group: Option<&str>) -> crate::scim::Directory {
        crate::scim::Directory {
            users: vec![crate::scim::User { id: "1".into(), user_name: user.into(), active, ..Default::default() }],
            groups: group.map(|g| crate::scim::Group { id: "g".into(), display_name: g.into(), members: vec!["1".into()], ..Default::default() }).into_iter().collect(),
            ..Default::default()
        }
    }

    #[test]
    fn the_directory_has_the_last_word_over_the_rules() {
        let rules = [rule("@example.com", false)];
        assert!(allowed(&Default::default(), &rules, "ana@example.com", &[]).is_some(), "nobody provisioned: the rules decide");
        assert!(allowed(&directory("ana@example.com", true, None), &rules, "ana@example.com", &[]).is_some());
        assert!(allowed(&directory("ana@example.com", false, None), &rules, "ana@example.com", &[]).is_none());
        let in_group = [rule("group:eng", false)];
        assert!(allowed(&directory("ana@example.com", true, Some("eng")), &in_group, "ana@example.com", &[]).is_some());
    }

    #[test]
    fn a_tailnet_member_answers_to_the_directory_too() {
        let everyone = [rule("*", false)];
        assert!(tailnet_scope_in(&Default::default(), &everyone, false, "ana@example.com").is_some());
        assert!(tailnet_scope_in(&Default::default(), &[], false, "ana@example.com").is_none());
        assert!(tailnet_scope_in(&Default::default(), &[], true, "ana@example.com").is_some(), "the whole tailnet");
        let off = directory("ana@example.com", false, None);
        assert!(tailnet_scope_in(&off, &everyone, true, "ana@example.com").is_none(), "deactivated: rules and tailnet-members don't matter");
        let mut deleted = crate::scim::Directory::default();
        deleted.removed.push("ana@example.com".into());
        assert!(tailnet_scope_in(&deleted, &everyone, true, "ana@example.com").is_none());
        // Groups from the directory count for tailnet members as they do for sign-ins.
        let in_group = [rule("group:eng", false)];
        assert!(tailnet_scope_in(&directory("ana@example.com", true, Some("eng")), &in_group, false, "ana@example.com").is_some());
    }
}
