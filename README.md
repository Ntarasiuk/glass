<p align="center">
  <a href="https://glassdesk.dev"><img src="site/public/icon-512.png" alt="Glass" width="96" height="96"></a>
</p>

<h1 align="center">Glass</h1>

<p align="center">
  <strong>Your computer, in any browser.</strong><br>
  A free, open-source remote desktop for <b>macOS</b> and <b>Linux</b> with about <b>20&nbsp;ms</b> of delay.<br>
  Control your Mac from an iPad, iPhone or any laptop. Nothing to install on the device in your hands.
</p>

<p align="center">
  <a href="https://github.com/Ntarasiuk/glass/releases/latest"><img alt="Latest release" src="https://img.shields.io/github/v/release/Ntarasiuk/glass?color=5cb2ff"></a>
  <a href="https://github.com/Ntarasiuk/glass/actions/workflows/ci.yml"><img alt="CI" src="https://github.com/Ntarasiuk/glass/actions/workflows/ci.yml/badge.svg"></a>
  <a href="LICENSE"><img alt="MIT license" src="https://img.shields.io/badge/license-MIT-6fdc8c"></a>
  <img alt="macOS 13+ and Linux (Wayland)" src="https://img.shields.io/badge/host-macOS%2013%2B%20%7C%20Linux%20Wayland-lightgrey">
  <img alt="Built with Rust" src="https://img.shields.io/badge/built%20with-Rust-orange">
</p>

<p align="center">
  <a href="https://glassdesk.dev">Website</a> ·
  <a href="https://glassdesk.dev/docs">Docs</a> ·
  <a href="https://glassdesk.dev/features">Features</a> ·
  <a href="#faq">FAQ</a>
</p>

---

```sh
curl -fsSL glassdesk.dev | sh
```

Run that on the computer you want to control. A guided setup connects [Tailscale](https://tailscale.com), installs the background service, checks each permission, then shows a QR code. Scan it with your iPad and you're in. About two minutes, start to finish.

## Why Glass

Most remote desktops ride the same pipeline as video calls, which buffers frames so motion looks smooth. That's fine for watching. It's wrong for typing, dragging and scrolling, where every buffered frame is lag you feel.

Glass is built like a game stream instead: hardware H.264 straight from the encoder, sent as unreliable QUIC datagrams so a lost packet never holds up a newer frame, decoded by the browser's hardware decoder in a worker, and drawn the moment it arrives. No jitter buffer. A live meter shows the real capture-to-display latency, typically **15–30 ms** on a home network or tailnet.

And the viewer is just a web page. No app to install, no account to create.

## Features

- **Feels local.** Hardware encode and decode, WebTransport datagrams, no buffering.
- **Just a browser.** iPad, iPhone, Mac, Windows or Linux. Add it to the Home Screen for a fullscreen app.
- **Full control.** Mouse, trackpad, keyboard, touch and pinch to zoom. ⌘Tab works on iPad. Unlock a locked Mac from the lock screen.
- **Every display.** Switch screens, or show them all side by side, arranged like your desk.
- **Every machine.** All your computers in one menu; switch without leaving the page.
- **Sound, clipboard, files.** System audio, copy and paste both ways, and direct file transfer.
- **From anywhere.** `glass link --anywhere` makes a one-time link that connects any browser straight to your computer over WebRTC: end-to-end encrypted, no ports opened, no Tailscale needed on the viewer.
- **Links that work once.** Let a phone or a friend's laptop in without sharing a password. Each device gets its own pass; `glass link --revoke` cuts them all off.
- **Steady on bad Wi-Fi.** Adapts bitrate, reconnects after sleep or tab suspension, and never shows a corrupted frame.
- **Private by default.** On your tailnet, your devices get in and nothing else does. Runs as your user, never root.
- **Agent-ready.** Ask Claude Code to "set up Glass on my Mac mini" and it does it over SSH ([skill](#set-up-with-an-ai-agent)).

## How it compares

| | Glass | Chrome Remote Desktop | Parsec, Moonlight, Jump Desktop | VNC / Screen Sharing |
|---|---|---|---|---|
| Viewer | Any browser | Any browser | Mostly native apps | Native app (or noVNC) |
| Pipeline | Game-stream style, no jitter buffer | Video-call (WebRTC) | Game-stream style | Pixel diffs |
| Mac and Linux hosts | ✅ (Linux beta) | ✅ | Varies | ✅ |
| Open source | ✅ MIT | ❌ | Moonlight + Sunshine only | Varies |
| Account required | No | Google | Usually | No |

## Install

**Host** (the computer you control):

- macOS 13 Ventura or newer, Apple Silicon or Intel.
- Linux x86_64 on Wayland with PipeWire (beta). Built on Hyprland/Omarchy; works on GNOME and KDE. Arch, Fedora 40+, Ubuntu 24.04+.

```sh
curl -fsSL glassdesk.dev | sh   # installs to ~/.glass, verifies the checksum, starts `glass setup`
```

Run it again any time to update. Prefer to read it first? [install.sh](site/public/install.sh). Release tarballs are on the [releases page](https://github.com/Ntarasiuk/glass/releases).

**Viewer:** any browser with WebTransport and WebCodecs: Safari 26.4+ (iPad, iPhone, Mac), Chrome, Edge or Firefox.

## Usage

```sh
glass setup              # guided setup: Tailscale, background service, permissions, QR code
glass status             # running? capturing? permissions? connect URL
glass link               # one-time link + QR code for another device
glass link --anywhere    # the same, from any network (WebRTC, end-to-end encrypted)
glass link --revoke      # cut off every device let in by a link
glass remote off         # turn off access from outside your tailnet
glass feedback "…"       # tell us what broke or what you want
```

On your tailnet, open `https://<computer>.<tailnet>.ts.net` on the iPad. Full CLI reference, keyboard tips for iPad and troubleshooting: [glassdesk.dev/docs](https://glassdesk.dev/docs).

## Set up with an AI agent

Glass ships an [agent skill](skills/glass/SKILL.md). With it, Claude Code finds a machine on your tailnet, installs Glass over SSH, runs the unattended setup, tells you which permission switch to click on that machine's screen, and verifies the connection.

```sh
mkdir -p ~/.claude/skills/glass && curl -fsSL https://glassdesk.dev/skill.md -o ~/.claude/skills/glass/SKILL.md
```

Then ask: *"set up Glass on my Mac mini"*. Scripts can use the same commands: `glass setup --yes --json` and `glass status --json`.

## How it works

```
Mac (glass)                                             Browser
ScreenCaptureKit ─▶ VideoToolbox H.264 ─▶ WebTransport ─▶ WebCodecs ─▶ OffscreenCanvas
 (IOSurface,         (hardware,           datagrams        (worker)
  on change)          low-latency RC)     over QUIC
CGEvent ◀───────────── input stream ◀──────────────────── pointer / keyboard / touch
NSPasteboard, files ◀─▶ control + file streams ◀─────────▶ clipboard, uploads, downloads
```

- **Capture** only when the screen changes, zero-copy. A still screen costs nothing.
- **Encode** in hardware, low-latency rate control, no B-frames. Keyframes on demand.
- **Transport:** video and audio as unreliable datagrams; input, clipboard and files on reliable streams. From anywhere, the same over a direct WebRTC connection that the host signs, so the browser refuses anyone else, glassdesk.dev included.
- **Decode** in a Web Worker straight to an OffscreenCanvas; the main thread only handles input.
- **Integrity:** every frame carries a checksum. Corrupted frames are never decoded; the client asks for a keyframe and recovers in a fraction of a second.
- **Linux:** xdg-desktop-portal + PipeWire capture, OpenH264, uinput, wl-clipboard, systemd user service.

| Path | What |
|---|---|
| [`crates/glass-host`](crates/glass-host) | The `glass` binary: WebTransport/HTTPS server, sessions, files, setup. Platform code in `src/macos/` and `src/linux/`. |
| [`crates/glass-proto`](crates/glass-proto) | Wire format, mirrored in `web/proto.js`. |
| [`web`](web) | Browser client. Plain JS, no build step. |
| [`site`](site) | glassdesk.dev: a Cloudflare Worker with D1, plus the WebRTC introducer (a Durable Object). |

## Security and privacy

- Everything is encrypted: TLS 1.3 over QUIC on your tailnet, end-to-end encrypted WebRTC from anywhere.
- A device gets in only if Tailscale says it's yours, or it has the access token or a one-time pass. Nothing is sent until then.
- Remote access from outside your tailnet is off until you make a link or run `glass remote on`.
- Glass runs as your user, never as root. The app sends nothing to glassdesk.dev except feedback you choose to send.

Found a security issue? Please open a [private security advisory](https://github.com/Ntarasiuk/glass/security/advisories/new) rather than a public issue.

## FAQ

**Is it free?** Yes, and MIT licensed.

**Do I need an app on the iPad?** No, just Safari. Tailscale is optional when you use `glass link --anywhere`.

**Can Windows be the host?** Not yet. Windows works as a viewer in any modern browser.

**Can I control a locked Mac?** Yes, type your password at the lock screen. After a reboot, FileVault still needs one local login.

**Do updates ask for permissions again?** No: releases are signed with a stable identity, so macOS keeps the grants. A build you made yourself, or Glass run from a terminal (which uses the terminal's permissions), is a different app to macOS and needs its own grant: run `glass setup`.

More in the [docs](https://glassdesk.dev/docs#faq).

## Development

Requirements: macOS 13+, stable Rust, and Node for the site and tests.

```sh
cargo run --release -p glass-host          # serves http://localhost:8080
```

The terminal you run it from needs Screen Recording and Accessibility permission. With Tailscale HTTPS enabled, other tailnet devices can connect at `https://<mac>.<tailnet>.ts.net:8080`. Logs from the host *and* connected browsers land in `logs/`, so you can debug an iPad without a debugger.

```sh
scripts/test.sh                             # everything: Rust, JS, end-to-end, site, Linux (if Docker is running)
scripts/test.sh e2e                         # just the browser end-to-end suite
./scripts/package.sh                        # universal release tarball in dist/
scripts/linux/build.sh [test]               # Linux x86_64 build/tests in an Arch container
cd site && npm install && npm run dev       # site locally (wrangler dev)
```

`glass --test` runs the host with fakes: two 640×360 test-pattern displays, recorded input, an in-memory clipboard and `/test/*` hooks to drop or corrupt frames. It never touches your real screen, so the Playwright suite in `tests/e2e` (real Chrome) covers connection, auth, input, touch, keyboard modes, clipboard, files, audio, display layouts, loss and corruption recovery, reconnects, machine switching and updates. CI runs it on macOS and Linux.

## Contributing

Issues and pull requests are welcome. For anything bigger than a fix, open an issue first so we can agree on the approach. Feedback also works from the app (`glass feedback "…"`) or at [glassdesk.dev](https://glassdesk.dev/features#feedback).

If Glass is useful to you, a ⭐ helps other people find it.

## License

[MIT](LICENSE) © Nathan Tarasiuk
