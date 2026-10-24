//! What the computer is playing (a track in Spotify, Music, a browser…), so a viewer's
//! media controls (Chrome's media hub, the iPad lock screen) show it and control it.
//!
//! macOS: a small helper (native/now-playing) loaded into /usr/bin/perl, since macOS only
//! answers MediaRemote for its own processes; it prints a JSON line per change and takes
//! commands on stdin. Linux: `playerctl` (MPRIS). Runs only while someone is connected.

use std::io::{BufRead, BufReader, Write};

use std::process::{Child, Command, Stdio};
use std::sync::Mutex;

use serde_json::{Value, json};
use tokio::sync::broadcast;

use crate::control::ServerMsg;

struct Helper {
    child: Child,
}

impl Drop for Helper {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

static HELPER: Mutex<Option<Helper>> = Mutex::new(None);
/// The latest report (artwork kept from the last report that carried one).
static LATEST: Mutex<Option<Value>> = Mutex::new(None);

/// What's playing now, for a viewer that just connected.
pub fn current() -> Option<Value> {
    LATEST.lock().unwrap().clone()
}

/// Starts the helper while anyone is connected, stops it when nobody is.
pub fn watch(events: broadcast::Sender<ServerMsg>) {
    let mut viewers = crate::viewers::subscribe();
    tokio::spawn(async move {
        loop {
            let connected = !viewers.borrow_and_update().is_empty();
            let running = HELPER.lock().unwrap().is_some();
            if connected && !running {
                start(events.clone());
            } else if !connected && running {
                stop();
            }
            if viewers.changed().await.is_err() {
                return;
            }
        }
    });
}

fn stop() {
    HELPER.lock().unwrap().take();
    *LATEST.lock().unwrap() = None;
}

/// How a source's lines become reports.
type Reader = fn(&str) -> Option<Value>;

fn start(events: broadcast::Sender<ServerMsg>) {
    let spawned = if crate::testmode::enabled() { test_source().map(|c| (c, parse as Reader)) } else { spawn() };
    let (mut child, read) = match spawned {
        Ok(c) => c,
        Err(e) => {
            tracing::debug!("now playing: unavailable ({e:#})");
            return;
        }
    };
    let Some(out) = child.stdout.take() else { return };
    *HELPER.lock().unwrap() = Some(Helper { child });
    std::thread::spawn(move || {
        for line in BufReader::new(out).lines() {
            let Ok(line) = line else { break };
            let Some(mut info) = read(&line) else { continue };
            let mut latest = LATEST.lock().unwrap();
            // A report without artwork keeps the last artwork, if it's the same track's.
            if info.get("artwork").is_none()
                && let Some(prev) = latest.as_ref()
                && prev["artworkId"] == info["artworkId"]
                && let Some(art) = prev.get("artwork")
            {
                info["artwork"] = art.clone();
                info["artworkType"] = prev["artworkType"].clone();
            }
            let send = strip_unchanged_artwork(&info, latest.as_ref());
            *latest = Some(info);
            drop(latest);
            let _ = events.send(ServerMsg::NowPlaying { info: send });
        }
    });
}

/// Sends the artwork only when it changed (it's the big part).
fn strip_unchanged_artwork(info: &Value, prev: Option<&Value>) -> Value {
    let mut out = info.clone();
    if prev.is_some_and(|p| p["artworkId"] == info["artworkId"] && p.get("artwork").is_some())
        && let Some(o) = out.as_object_mut()
    {
        o.remove("artwork");
        o.remove("artworkType");
    }
    out
}

#[cfg(target_os = "macos")]
fn spawn() -> anyhow::Result<(Child, Reader)> {
    const SCRIPT: &str = r#"use DynaLoader; my $l = DynaLoader::dl_load_file($ARGV[0]) or die DynaLoader::dl_error(); my $s = DynaLoader::dl_find_symbol($l, "glass_now_playing") or die "no symbol"; DynaLoader::dl_install_xsub("main::run", $s); run();"#;
    let lib = helper_path().ok_or_else(|| anyhow::anyhow!("no now-playing helper in this build"))?;
    let child = Command::new("/usr/bin/perl")
        .args(["-e", SCRIPT])
        .arg(lib)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    Ok((child, parse))
}

/// `<prefix>/share/glass/libglassnowplaying.dylib`, or a checkout's build in target/.
#[cfg(target_os = "macos")]
fn helper_path() -> Option<std::path::PathBuf> {
    const NAME: &str = "libglassnowplaying.dylib";
    if let Some(p) = std::env::var_os("GLASS_NOW_PLAYING_HELPER") {
        return Some(p.into());
    }
    let exe = std::env::current_exe().ok()?.canonicalize().ok()?;
    let root = exe.parent()?.parent()?;
    [root.join("share/glass").join(NAME), root.join("now-playing").join(NAME)]
        .into_iter()
        .find(|p| p.exists())
}

/// Linux: MPRIS through `playerctl`, one tab-separated line per change.
#[cfg(target_os = "linux")]
fn spawn() -> anyhow::Result<(Child, Reader)> {
    let format = "{{status}}\t{{title}}\t{{artist}}\t{{album}}\t{{mpris:length}}\t{{position}}\t{{mpris:trackid}}";
    let child = Command::new("playerctl")
        .args(["--follow", "metadata", "--format", format])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    Ok((child, parse_playerctl))
}

#[cfg(target_os = "linux")]
fn parse_playerctl(line: &str) -> Option<Value> {
    let f: Vec<&str> = line.split('\t').collect();
    let secs = |i: usize| f.get(i).and_then(|v| v.parse::<f64>().ok()).map(|us| us / 1e6);
    let text = |i: usize| f.get(i).filter(|s| !s.is_empty()).map(|s| s.to_string());
    let playing = f.first() == Some(&"Playing");
    parse(
        &json!({
            "playing": playing,
            "rate": if playing { 1 } else { 0 },
            "title": text(1),
            "artist": text(2),
            "album": text(3),
            "duration": secs(4),
            "elapsed": secs(5),
            "artworkId": f.get(6).unwrap_or(&""),
        })
        .to_string(),
    )
}

#[cfg(not(any(target_os = "macos", target_os = "linux")))]
fn spawn() -> anyhow::Result<(Child, Reader)> {
    anyhow::bail!("not supported here")
}

/// Tests: one fixed track; commands are recorded.
fn test_source() -> anyhow::Result<Child> {
    let track = json!({
        "title": "Gravity", "artist": "John Mayer", "album": "Continuum",
        "playing": true, "rate": 1, "duration": 245.0, "elapsed": 12.0, "artworkId": "test",
        "artwork": "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==",
        "artworkType": "image/png",
    });
    Ok(Command::new("sh")
        .args(["-c", &format!("printf '%s\\n' '{track}'; cat >/dev/null")])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()?)
}

fn parse(line: &str) -> Option<Value> {
    let v: Value = serde_json::from_str(line).ok()?;
    // Nothing playing at all (no app has a track): say so as `null`-ish.
    if v.get("title").is_none_or(Value::is_null) && v.get("artist").is_none_or(Value::is_null) {
        return Some(json!({ "artworkId": "" }));
    }
    Some(v)
}

/// A media control from a viewer: play, pause, toggle, next, previous, or seek.
pub fn command(action: &str, position: Option<f64>) {
    let line = match action {
        "play" | "pause" | "toggle" | "next" | "previous" => action.to_string(),
        "seek" => match position {
            Some(p) if p.is_finite() && p >= 0.0 => format!("seek {p:.3}"),
            _ => return,
        },
        _ => return,
    };
    if crate::testmode::enabled() {
        crate::testmode::record("media", json!(line));
        return;
    }
    #[cfg(target_os = "macos")]
    if let Some(h) = HELPER.lock().unwrap().as_mut()
        && let Some(stdin) = h.child.stdin.as_mut()
    {
        let _ = writeln!(stdin, "{line}");
    }
    #[cfg(target_os = "linux")]
    {
        let args: Vec<String> = match line.split_once(' ') {
            Some(("seek", p)) => vec!["position".into(), p.into()],
            _ => vec![if line == "toggle" { "play-pause".into() } else { line.clone() }],
        };
        let _ = Command::new("playerctl").args(&args).status();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn artwork_is_sent_once_per_track() {
        let a = json!({ "title": "A", "artworkId": "1", "artwork": "xx" });
        assert!(strip_unchanged_artwork(&a, None).get("artwork").is_some());
        assert!(strip_unchanged_artwork(&a, Some(&a)).get("artwork").is_none());
        let b = json!({ "title": "B", "artworkId": "2", "artwork": "yy" });
        assert!(strip_unchanged_artwork(&b, Some(&a)).get("artwork").is_some());
    }

    #[test]
    fn nothing_playing_is_empty() {
        assert_eq!(parse("{}").unwrap(), json!({ "artworkId": "" }));
        assert_eq!(parse(r#"{"title":"T","artworkId":"x"}"#).unwrap()["title"], "T");
        assert!(parse("not json").is_none());
    }
}
