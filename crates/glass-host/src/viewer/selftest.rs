//! `glass viewer-selftest <test host>`: drives every feature of the native viewer against a
//! `glass --test` host, through the same session API the window uses, and checks what the
//! host saw (its /test endpoints). Sound plays silently; this Mac's clipboard is put back.

use std::time::{Duration, Instant};

use anyhow::{Result, bail};
use base64::Engine;
use glass_proto::{GAMEPAD_BUTTON, InputEvent};
use serde_json::{Value, json};

use super::{Session, Status, Target};

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder().timeout_global(Some(Duration::from_secs(5))).build().new_agent()
}

fn get(base: &str, path: &str) -> Result<Value> {
    Ok(agent().get(format!("{base}{path}")).call()?.body_mut().read_json()?)
}

fn get_text(base: &str, path: &str) -> Result<String> {
    Ok(agent().get(format!("{base}{path}")).call()?.body_mut().read_to_string()?)
}

/// Waits for a condition, polling.
async fn until(secs: u64, mut f: impl FnMut() -> bool) -> bool {
    let end = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < end {
        if f() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    f()
}

/// Collects inbox messages while waiting for one that matches.
async fn message(session: &Session, seen: &mut Vec<Value>, secs: u64, f: impl Fn(&Value) -> bool) -> Option<Value> {
    let end = Instant::now() + Duration::from_secs(secs);
    loop {
        seen.extend(session.viewer.take_inbox());
        if let Some(m) = seen.iter().find(|m| f(m)) {
            return Some(m.clone());
        }
        if Instant::now() > end {
            return None;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

pub async fn run(base: &str, token: String) -> Result<()> {
    // SAFETY: before any other thread reads it (the sound thread starts later).
    unsafe { std::env::set_var("GLASS_VIEWER_MUTE", "1") };
    let base = base.trim_end_matches('/').to_string();
    let mut target = Target::from_url(&base).unwrap();
    target.token = Some(token);
    let session = Session::start(&tokio::runtime::Handle::current(), target);
    session.viewport(&[[1280, 720]]);
    let mut results: Vec<(&str, bool, String)> = Vec::new();
    let mut seen = Vec::new();
    let live = until(10, || session.viewer.status() == Status::Live).await;
    results.push(("connect", live, format!("{:?}", session.viewer.status())));
    if !live {
        bail!("never connected");
    }
    let _ = agent().delete(format!("{base}/test/input")).call();

    // Sound: the test tone arrives, is decoded and goes out to the speakers (muted).
    session.audio(true);
    let played0 = super::audio::PLAYED.load(std::sync::atomic::Ordering::Relaxed);
    let ok = until(4, || session.viewer.live.lock().unwrap().audio_packets > 100 && super::audio::PLAYED.load(std::sync::atomic::Ordering::Relaxed) > played0 + 4800).await;
    results.push(("sound", ok, format!("{} packets, {} frames played", session.viewer.live.lock().unwrap().audio_packets, super::audio::PLAYED.load(std::sync::atomic::Ordering::Relaxed) - played0)));

    // Microphone: this Mac's mic as KIND_MIC packets (silence without permission, still sent).
    let mics = || get(&base, "/test/records?kind=mic").map(|v| v["records"].as_array().map_or(0, |a| a.len())).unwrap_or(0);
    let before = mics();
    session.mic(true);
    let ok = until(4, || mics() > before + 20).await;
    let error = seen.iter().chain(session.viewer.take_inbox().iter()).find(|m| m["t"] == "_error").map(|m| m["msg"].to_string());
    results.push(("microphone", ok, error.unwrap_or_else(|| format!("{} packets reached the host", mics() - before))));
    session.mic(false);

    // Clipboard, this Mac → there: text, then an image.
    session.control(json!({ "t": "clipboard", "text": "from the Mac", "html": "<b>from the Mac</b>" }));
    let ok = until(3, || get_text(&base, "/test/clipboard").is_ok_and(|t| t == "from the Mac")).await;
    results.push(("clipboard to host", ok, get_text(&base, "/test/clipboard").unwrap_or_default()));
    let png = crate::viewer::selftest::tiny_png();
    session.clipboard_image("image/png".into(), png.clone(), "pic".into(), false);
    let ok = until(3, || get(&base, "/test/clipboard/rich").is_ok_and(|v| v["image"].as_str().and_then(|i| base64::engine::general_purpose::STANDARD.decode(i).ok()) == Some(png.clone()))).await;
    results.push(("clipboard image to host", ok, String::new()));

    // Clipboard, there → this Mac: the host's change arrives, its image on request.
    let _ = agent().post(format!("{base}/test/clipboard/rich")).send_json(json!({ "text": "from the host", "image": base64::engine::general_purpose::STANDARD.encode(&png) }));
    let m = message(&session, &mut seen, 3, |m| m["t"] == "clipboard" && m["text"] == "from the host").await;
    let ok = m.as_ref().is_some_and(|m| m["image"] == true);
    session.control(json!({ "t": "clipboardImage", "id": 1 }));
    let got = message(&session, &mut seen, 3, |m| m["t"] == "_clipboardImage").await.is_some() && session.viewer.take_clip_image().is_some_and(|(_, b)| !b.is_empty());
    results.push(("clipboard from host", ok && got, String::new()));

    // And onto this Mac's pasteboard (the window's code), putting what was there back.
    let before = super::sys::clipboard();
    crate::clipboard::set(&crate::clip::Clip { text: "glass selftest".into(), html: None, image: Some(png.clone()) });
    let (text, _, image) = super::sys::clipboard();
    crate::clipboard::set(&crate::clip::Clip { text: before.0.clone(), html: before.1.clone(), image: None });
    results.push(("this computer's clipboard", text == "glass selftest" && image, format!("restored {} chars", before.0.len())));

    // Chat: sent, and everyone (this viewer too) hears it.
    session.control(json!({ "t": "chat", "text": "hello from the app" }));
    let ok = message(&session, &mut seen, 3, |m| m["t"] == "chat" && m["text"] == "hello from the app").await.is_some();
    results.push(("chat", ok, String::new()));

    // Terminal: a shell, typed into, and its output.
    session.control(json!({ "t": "term", "op": "open", "cols": 80, "rows": 24 }));
    let input = base64::engine::general_purpose::STANDARD.encode(b"echo glass-$((40+2))\r");
    tokio::time::sleep(Duration::from_millis(500)).await;
    session.control(json!({ "t": "term", "op": "input", "data": input }));
    let mut out = String::new();
    let ok = until(5, || {
        seen.extend(session.viewer.take_inbox());
        out = seen
            .iter()
            .filter(|m| m["t"] == "term" && m["op"] == "output")
            .filter_map(|m| m["data"].as_str().and_then(|d| base64::engine::general_purpose::STANDARD.decode(d).ok()))
            .map(|b| String::from_utf8_lossy(&b).to_string())
            .collect();
        out.contains("glass-42")
    })
    .await;
    // The same bytes through the window's screen (VT100), to show it reads right.
    let mut parser = vt100::Parser::new(24, 80, 0);
    parser.process(out.as_bytes());
    let screen = parser.screen().contents();
    results.push(("terminal", ok && screen.contains("glass-42"), screen.lines().find(|l| l.contains("glass-42")).unwrap_or("").trim().to_string()));
    session.control(json!({ "t": "term", "op": "close" }));

    // Game controller and captured mouse: what the window sends for them.
    session.input(InputEvent::Gamepad { pad: 0, kind: glass_proto::GAMEPAD_CONNECTED, index: 0, value: 1.0 });
    session.input(InputEvent::Gamepad { pad: 0, kind: GAMEPAD_BUTTON, index: 0, value: 1.0 });
    session.input(InputEvent::MouseMoveBy { dx: 12.0, dy: -3.0 });
    let ok = until(3, || {
        get(&base, "/test/input").is_ok_and(|v| {
            let ev = v["events"].as_array().cloned().unwrap_or_default();
            ev.iter().any(|e| e["t"] == "gamepad" && e["kind"] == 0 && e["value"] == 1.0) && ev.iter().any(|e| e["t"] == "moveBy" && e["dx"] == 12.0)
        })
    })
    .await;
    results.push(("gamepad + captured mouse", ok, String::new()));
    // This Mac's controllers, read the way the window reads them.
    let mut pads = super::gamepad::Pads::default();
    let events = std::cell::Cell::new(0);
    pads.poll(&|_| events.set(events.get() + 1));
    results.push(("controllers on this computer", true, format!("{} connected, {} events", if super::gamepad::Pads::any() { "some" } else { "none" }, events.get())));

    // Display previews.
    session.control(json!({ "t": "thumbs" }));
    let ok = message(&session, &mut seen, 3, |m| m["t"] == "thumbs" && m["thumbs"].as_array().is_some_and(|a| !a.is_empty())).await.is_some();
    results.push(("display previews", ok, String::new()));

    println!();
    let mut failed = 0;
    for (name, ok, detail) in &results {
        println!("{} {name}{}", if *ok { "✓" } else { "✗" }, if detail.is_empty() { String::new() } else { format!(" — {detail}") });
        failed += !ok as usize;
    }
    if failed > 0 {
        bail!("{failed} failed");
    }
    Ok(())
}

/// A 2×2 PNG.
pub fn tiny_png() -> Vec<u8> {
    let mut out = Vec::new();
    let mut enc = png::Encoder::new(&mut out, 2, 2);
    enc.set_color(png::ColorType::Rgba);
    enc.set_depth(png::BitDepth::Eight);
    let mut w = enc.write_header().unwrap();
    w.write_image_data(&[255, 0, 0, 255, 0, 255, 0, 255, 0, 0, 255, 255, 255, 255, 255, 255]).unwrap();
    drop(w);
    out
}
