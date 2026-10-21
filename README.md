# Glass

Low-latency remote desktop for macOS and Linux (Wayland, beta). Install it on the computer you want to control, then see and control it from a browser on an iPad, iPhone or another computer, typically 15–30 ms from screen capture to display.

```sh
curl -fsSL glassdesk.dev | sh
```

Run that on the computer you want to control. It installs Glass and starts `glass setup`, which walks you through Tailscale, the background service and macOS permissions, then shows a QR code to open on your iPad. Full docs: [glassdesk.dev/docs](https://glassdesk.dev/docs) (also as [markdown](site/docs.md)).

## Features

- **Low latency.** Hardware H.264, WebTransport datagrams, WebCodecs decode in a worker, no jitter buffer.
- **Just a browser** on the viewing device. Add it to the Home Screen for a fullscreen app.
- **Full control.** Mouse, trackpad, keyboard and touch, including at the lock screen. ⌘Tab via "Ctrl as ⌘" on iPad.
- **Multiple displays.** Switch, or show all of them side by side, arranged like System Settings.
- **Audio, clipboard sync and file transfer** in both directions.
- **Private.** Runs over your Tailscale tailnet. Your own devices are let in automatically; anything else needs a token.
- **Resilient.** Reconnects after tab suspension, sleep or restarts. Corrupted frames are detected by checksum and never decoded.

## How it works

```
Mac (glass)                                             Browser
ScreenCaptureKit ─▶ VideoToolbox H.264 ─▶ WebTransport ─▶ WebCodecs ─▶ OffscreenCanvas
 (IOSurface,         (hardware,           datagrams        (worker)
  on change)          low-latency RC)     over QUIC
CGEvent ◀───────────── input stream ◀──────────────────── pointer / keyboard / touch
NSPasteboard, files ◀─▶ control + file streams ◀─────────▶ clipboard, uploads, downloads
```

- `crates/glass-proto`: wire format (mirrored in `web/proto.js`).
- `crates/glass-host`: the `glass` binary: WebTransport/HTTPS server, sessions, files, setup wizard. Platform code lives in `src/macos/` (ScreenCaptureKit, VideoToolbox, CGEvent, launchd) and `src/linux/` (portal + PipeWire, OpenH264, uinput, wl-clipboard, systemd).
- `web/`: browser client, no build step.
- `site/`: glassdesk.dev, a Cloudflare Worker with static assets plus D1 for download counts and feedback.

## Development

Requirements: macOS 13+, Rust (stable), and Node for the site.

```sh
cargo run --release -p glass-host          # serves http://localhost:8080
open http://localhost:8080
```

The process you run it from (your terminal) needs Screen Recording and Accessibility permission. With Tailscale HTTPS enabled, the host fetches a certificate automatically, so other tailnet devices can connect at `https://<mac>.<tailnet>.ts.net:8080`.

Logs from both the host and connected browsers go to `logs/` (`host.log`, `client.log`), which makes it easy to debug issues on an iPad without a debugger attached.

```sh
cargo test                                  # protocol and host unit tests
./scripts/package.sh                        # universal release tarball in dist/
scripts/linux/build.sh [test]               # Linux x86_64 build/tests in an Arch container
cd site && npm install && npm run dev       # site locally (wrangler dev)
```

## Testing

```sh
scripts/test.sh            # everything: Rust, JS, end-to-end, site, Linux (if Docker is running)
scripts/test.sh e2e        # just the browser end-to-end suite
```

`glass --test` runs the host with fakes: a test pattern on two 640×360 displays instead of screen capture, recorded input instead of injection, an in-memory clipboard and file chooser, and `/test/*` hooks to drop or corrupt frames. It never touches your real screen, mouse or clipboard, so the Playwright suite in `tests/e2e` (real Chrome) can exercise connection, auth, input, touch, keyboard modes, clipboard, files, audio, display layouts, loss and corruption recovery, reconnects, machine switching and updates. CI runs it on macOS and Linux.

## Status

Early but daily-driven. Known rough edges:

- Updates ask for Screen Recording and Accessibility again, because unsigned binaries change identity on each build. Code signing would fix it.
- Safari's hardware video decoder occasionally resets. Glass recovers in a fraction of a second and falls back to steadier decoder settings.

Feedback: `glass feedback "…"`, the form at [glassdesk.dev](https://glassdesk.dev/#feedback), or GitHub issues.

## License

[MIT](LICENSE)
