//! The computers this app has opened, kept so they can be opened again from wherever this
//! one is: at home they're heard on the network, but over a VPN into it nothing is heard, and the address they had still reaches them.
//! Kept in `computers.json` in the config folder.
//!
//! An address is only where to look. What's there has to prove it's the same computer
//! before it's shown a pass or a token: by the key it gave the first time ([`Computer::key`],
//! which signs the certificate about to be pinned), or by a certificate for its name.

use std::net::IpAddr;

use serde::{Deserialize, Serialize};

/// How many addresses a computer is remembered at (home, the office, …), newest first.
const ADDRS: usize = 4;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Computer {
    /// Its id on the network (`nearby`), the same wherever it's reached.
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub os: String,
    /// What it's remembered by (`prefs`): `http://glass-<id>.local:8080`, or its
    /// certificate's name.
    pub url: String,
    /// The key it proves itself with (`remote::HostKey::direct`).
    pub key: String,
    /// Where it has been reached, newest first.
    pub addrs: Vec<IpAddr>,
    /// What it's called on its network's DNS (`studio.lan`, `studio`), newest first: where
    /// to look when it's at none of those addresses any more (`net::reach`).
    #[serde(default)]
    pub names: Vec<String>,
    /// When it was last opened (unix seconds).
    #[serde(default)]
    pub last: u64,
}

fn path() -> std::path::PathBuf {
    crate::config_dir().join("computers.json")
}

/// Every remembered computer, by name.
pub fn all() -> Vec<Computer> {
    let mut list: Vec<Computer> = std::fs::read(path()).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
    list.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()).then(a.id.cmp(&b.id)));
    list
}

/// The computer remembered by this address, if any.
pub fn by_url(url: &str) -> Option<Computer> {
    all().into_iter().find(|c| c.url == url)
}

fn store(list: &[Computer]) {
    if let Ok(json) = serde_json::to_vec_pretty(list) {
        let _ = std::fs::create_dir_all(crate::config_dir());
        let _ = super::prefs::write_private(&path(), &json);
    }
}

/// Remembers a computer just opened at `at` (or that it's there now).
pub fn remember(seen: Computer, at: IpAddr) {
    let mut list = all();
    store(merge(&mut list, seen, at));
}

fn merge(list: &mut Vec<Computer>, mut seen: Computer, at: IpAddr) -> &[Computer] {
    // The same computer under an older address of its own (it got a certificate, say).
    let old = list.iter().position(|c| c.id == seen.id).map(|i| list.remove(i));
    let (addrs, names) = old.map(|c| (c.addrs, c.names)).unwrap_or_default();
    fn newest<T: PartialEq>(all: impl Iterator<Item = T>) -> Vec<T> {
        all.fold(Vec::new(), |mut kept, one| {
            if !kept.contains(&one) && kept.len() < ADDRS {
                kept.push(one);
            }
            kept
        })
    }
    seen.addrs = newest(std::iter::once(at).chain(addrs));
    seen.names = newest(std::mem::take(&mut seen.names).into_iter().chain(names));
    list.push(seen);
    list
}

/// Forgets a computer, and what this app held for it (its pass or token).
pub fn forget(id: &str) {
    let mut list = all();
    if let Some(i) = list.iter().position(|c| c.id == id) {
        let gone = list.remove(i);
        super::prefs::update(&gone.url, |m| (m.pass, m.token) = (None, None));
        store(&list);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn computer(id: &str, url: &str) -> Computer {
        Computer { id: id.into(), name: "Studio".into(), os: "macos".into(), url: url.into(), key: "k".into(), addrs: Vec::new(), names: Vec::new(), last: 1 }
    }

    #[test]
    fn a_computer_is_remembered_once_at_its_newest_address_first() {
        let ip = |s: &str| s.parse::<IpAddr>().unwrap();
        let mut list = Vec::new();
        merge(&mut list, computer("a", "http://glass-a.local:8080"), ip("192.168.1.20"));
        merge(&mut list, computer("b", "http://glass-b.local:8080"), ip("192.168.1.21"));
        merge(&mut list, computer("a", "https://studio.example.ts.net:8080"), ip("10.0.0.9"));
        merge(&mut list, computer("a", "https://studio.example.ts.net:8080"), ip("192.168.1.20"));
        assert_eq!(list.len(), 2);
        let a = list.iter().find(|c| c.id == "a").unwrap();
        assert_eq!(a.url, "https://studio.example.ts.net:8080", "its newest name");
        assert_eq!(a.addrs, [ip("192.168.1.20"), ip("10.0.0.9")]);
        let named = |names: &[&str]| Computer { names: names.iter().map(|n| n.to_string()).collect(), ..computer("a", "http://glass-a.local:8080") };
        merge(&mut list, named(&["studio.lan", "studio"]), ip("192.168.1.20"));
        merge(&mut list, named(&["studio", "den"]), ip("192.168.1.20"));
        assert_eq!(list.iter().find(|c| c.id == "a").unwrap().names, ["studio", "den", "studio.lan"]);
        for n in 0..9 {
            merge(&mut list, computer("a", "http://glass-a.local:8080"), ip(&format!("10.1.1.{n}")));
        }
        assert_eq!(list.iter().find(|c| c.id == "a").unwrap().addrs.len(), ADDRS);
    }
}
