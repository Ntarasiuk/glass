# Glass

> Glass is a free, open-source, low-latency remote desktop for macOS and Linux (Wayland). Install it on the computer you want to control, then see and control it from a browser on an iPad, iPhone, or another computer. Typical latency is 15–30 ms from screen capture to display, including over Tailscale. Nothing to install on the viewing device.

- Website: https://glassdesk.dev
- Install (on the computer to be controlled): `curl -fsSL glassdesk.dev | sh`
- Hosts: macOS 13+ (Apple Silicon and Intel), and Linux x86_64 on Wayland (beta; built for Hyprland/Omarchy, should work on GNOME and KDE).
- Viewers: any modern browser with WebTransport and WebCodecs: Safari 26.4+ (iPad, iPhone, Mac), Chrome, Chromium, Edge and Firefox, on any OS including Linux.
- Price: free and open source (MIT): https://github.com/Ntarasiuk/glass

## What Glass does

Glass streams a Mac's screen to a web browser and sends mouse, keyboard, and touch input back. It is built for speed, using the same techniques as game-streaming tools (hardware video encoding, UDP-style transport, no buffering) instead of the video-call stack that tools like Chrome Remote Desktop use.

Features:

- **Low latency.** About 15–30 ms capture-to-display on a home network or Tailscale, measured live in the stats overlay.
- **Works in the browser.** Open a URL on the iPad; there's no app to install. Add it to the Home Screen for a true fullscreen app.
- **Full control.** Mouse, trackpad, keyboard, and touch (tap to click, drag, long-press to right-click, two-finger scroll), plus an on-screen keyboard button.
- **Unlocks a locked Mac.** Type the password at the lock screen from the remote device.
- **Multiple displays.** Show one display, switch between them, or show all of them side by side in their macOS arrangement.
- **Audio.** Streams Mac system audio to the browser.
- **Clipboard sync.** Copy on the Mac, paste on the iPad, and the reverse.
- **File transfer.** Send files from the iPad to the Mac's Downloads folder, or pick files on the Mac and save them on the iPad. Transfers are direct and peer-to-peer.
- **Auto-reconnect.** Survives iPad tab suspension, network changes, and host restarts.
- **Self-updating client.** Open tabs reload when the Mac is updated.
- **Secure by default.** Devices on your own Tailscale tailnet are let in automatically; anything else needs an access token.

## Install

On the Mac you want to control, run in Terminal:

```sh
curl -fsSL glassdesk.dev | sh
```

The installer:

1. Checks that this is macOS 13 or newer.
2. Downloads the latest release (a universal binary for Apple Silicon and Intel) and verifies its SHA-256 checksum.
3. Installs to `~/.glass` and links the `glass` command into `~/.local/bin` (adding that to your PATH if needed).
4. Starts `glass setup`, the interactive setup guide.

Running the same command again updates Glass in place and restarts the background service.

## Setup (`glass setup`)

The setup guide walks through four steps and checks each one live:

1. **Tailscale (remote access).** Glass uses [Tailscale](https://tailscale.com) to reach the Mac securely from other devices, with no port forwarding. Setup checks that Tailscale is installed and signed in, and that **HTTPS Certificates** are enabled for your tailnet (one switch on https://login.tailscale.com/admin/dns; browsers require HTTPS for the APIs Glass uses). It then fetches a certificate for the Mac's tailnet name. You can skip this to use Glass only on the Mac itself.
2. **Background service.** Glass installs a per-user LaunchAgent (`dev.glassdesk.host`) so it runs in the background, optionally starts at login, and restarts if it crashes.
3. **Screen Recording permission.** macOS asks to let `glass` record the screen. Setup opens the right System Settings pane and waits until capture works.
4. **Accessibility permission.** Needed to inject mouse and keyboard input. Setup opens the pane and waits until it's allowed.

It finishes by showing the connect URL and a QR code to scan with the iPad.

## Connecting

1. Install Tailscale on the iPad (or other device) and sign in with the same account as the Mac.
2. Open `https://<your-mac>.<your-tailnet>.ts.net:8080` (shown at the end of setup, and by `glass status`).
3. Optional: in Safari or Chrome, tap **Share → Add to Home Screen**, then launch Glass from its icon. It runs fullscreen with no browser UI, so Safari's own shortcuts (⌘W, ⌘T, ⌘L, ⌘R) reach the Mac instead.

Devices that aren't on your tailnet, or belong to another tailnet user, are asked for the access token from `~/.config/glass/token`. A link containing `#t=<token>` signs a browser in once and is then remembered.

## Using Glass

The toolbar (top right, fades when idle) has:

- **Displays:** pick one display, or **All displays** to show every screen side by side in the same arrangement as System Settings → Displays.
- **⌘ (Mac shortcuts):** one-tap app switcher, Spotlight, Mission Control, Hide, Quit, Close window, screenshot, Force Quit, and Lock Screen. Also keyboard remapping options (see below).
- **Keyboard:** brings up the iPad's on-screen keyboard.
- **Send files to Mac:** uploads to `~/Downloads` (drag and drop also works).
- **Get files from Mac:** opens a file chooser on the Mac's screen; chosen files arrive with a Save button.
- **Audio:** turns Mac system audio on or off for this device.
- **Fullscreen** (hidden when running as a Home Screen app) and **Stats**.

Touch gestures: tap = click, drag = drag, long-press = right-click, two fingers = scroll.

Local shortcuts (not sent to the Mac): Ctrl+Shift+F toggles fullscreen, Ctrl+Shift+H toggles the stats overlay.

### Keyboard and ⌘ shortcuts on iPad

iPadOS reserves a few shortcuts before any app or web page sees them: ⌘Tab, ⌘Space, ⌘H, ⌘⇧3/4/5, and Globe-key combinations. Glass offers three ways around this:

- **Ctrl as ⌘** (on by default): Ctrl+Tab, Ctrl+Space, Ctrl+H, and Ctrl+⇧3/4/5 are sent to the Mac as their ⌘ versions. Holding Ctrl and tapping Tab cycles apps exactly like ⌘Tab. Every other Ctrl combination stays Ctrl.
- **Swap ⌘ and ⌥:** pair with iPad Settings → General → Keyboard → Hardware Keyboard → Modifier Keys (Command → Option, Option → Command). The physical ⌘ key then reaches the Mac as ⌘ even for reserved shortcuts.
- **⌘ menu:** one-tap buttons for the reserved shortcuts.

Everything else, including ⌘C, ⌘V, ⌘Q, ⌘W, and arrows, works directly from a hardware keyboard.

### Clipboard

- ⌘C or ⌘X on the Mac (from the iPad keyboard) puts the Mac's copied text on the iPad's clipboard.
- ⌘V pastes the iPad's clipboard into the Mac.
- Any Mac clipboard change is sent to connected devices.

## CLI reference

```
glass               Run the host in the foreground (the background service runs this)
glass setup         Guided setup: Tailscale, background service, permissions, connect
glass status        Is the service running? Capturing? Permissions? Connect URL
glass feedback "…"  Send feedback to the developers
glass uninstall     Stop the background service and remove it from login items
glass --version
```

Host options: `--http-port` (default 8080), `--wt-port` (UDP, default 4433), `--fps` (default 60), `--mbps` (default 30), `--scale`, `--cert`/`--key` (custom TLS), `--token-file`, `--web-dir`, `--log-dir`.

Files:

- `~/.glass`: installed binary and web client
- `~/.config/glass/token`: access token (created on first run)
- `~/.config/glass/certs/`: Tailscale HTTPS certificate
- `~/.config/glass/status.json`: live service status
- `~/Library/Logs/Glass/`: host, client, and agent logs
- `~/Library/LaunchAgents/dev.glassdesk.host.plist`: background service

## How it works

- **Capture:** ScreenCaptureKit, zero-copy IOSurface frames, only when the screen changes. A static screen costs nothing.
- **Encode:** VideoToolbox hardware H.264 in low-latency rate-control mode, with no B-frames. Keyframes are sent on demand (new viewer or detected loss) and at least every 900 frames.
- **Transport:** WebTransport (HTTP/3 over QUIC). Video and audio travel as unreliable datagrams, so a lost packet never blocks newer ones. Input, control, clipboard, and files use reliable streams.
- **Decode and render:** WebCodecs hardware decoding in a Web Worker, drawn to an OffscreenCanvas. The page's main thread only handles input.
- **Audio:** 48 kHz PCM in 5 ms packets, played through an AudioWorklet with a ~40 ms jitter buffer.
- **Integrity and recovery:** each frame carries a checksum; corrupted or incomplete frames are never decoded. The client requests a keyframe and recovers within a fraction of a second. If the browser's hardware decoder misbehaves, Glass falls back to more conservative decoder settings and remembers the choice.
- **Clock sync:** NTP-style ping/pong over datagrams, used for the live latency display.

## Linux host (beta)

The same one-liner installs Glass on Linux x86_64. The Linux host uses:

- **Screen capture:** xdg-desktop-portal ScreenCast over PipeWire. On Hyprland (Omarchy), the share picker asks once which screen to share; Glass saves a restore token so it doesn't ask again.
- **Encoding:** software H.264 (OpenH264, screen-content mode). Screens larger than 2560×1600 are halved before encoding, to keep the frame rate up. Hardware VA-API encoding is planned.
- **Input:** a virtual keyboard and absolute pointer through `/dev/uinput`. `glass setup` installs a one-line udev rule (`/etc/udev/rules.d/60-glass-uinput.rules`) giving the logged-in user access; this needs `sudo` once.
- **Clipboard:** `wl-copy` / `wl-paste` (wl-clipboard).
- **Audio:** the default output's monitor, through PipeWire.
- **Background service:** a systemd user unit (`~/.config/systemd/user/glass.service`) tied to the graphical session. Logs: `journalctl --user -u glass` and `~/.local/state/glass/`.
- **Tailscale:** non-root users need to be Tailscale operators to fetch HTTPS certificates; setup offers to run `sudo tailscale set --operator=$USER`.

Dependencies on Arch / Omarchy: `sudo pacman -S --needed pipewire xdg-desktop-portal-hyprland wl-clipboard tailscale`. Setup checks for these.

Keys arrive as typed on Linux hosts: ⌘ on an Apple keyboard is Super. Paste from the viewer's clipboard sends Ctrl+V.

## Viewing from Linux or Windows (Omarchy included)

Open the Glass URL in Chromium, Chrome, or Firefox with Tailscale running. When the host is a Mac, **PC keyboard** mode (on by default for Linux and Windows browsers; toggle it in the ⌘ menu) translates PC conventions:

- Ctrl → ⌘ (so Ctrl+C, Ctrl+V, Ctrl+Tab, and Ctrl+Q do what you expect on the Mac)
- Super → Ctrl
- Alt → ⌥

Hyprland keeps its own Super shortcuts. In fullscreen (Ctrl+Shift+F), Chromium's Keyboard Lock passes everything else through. If the browser has no hardware H.264 decoder, Glass uses software decoding automatically.

## Security and privacy

- All traffic is encrypted (TLS 1.3 via QUIC/HTTPS).
- With Tailscale, traffic stays on your private tailnet and goes directly between devices when possible.
- Access control: a device is let in if Tailscale reports it belongs to the same user as the Mac, or if it presents the access token. Glass sends nothing to a session (video, audio, clipboard, input) until it is authenticated.
- Glass runs as your user (a LaunchAgent on macOS, a systemd user service on Linux), never as root.
- The glassdesk.dev website counts downloads using a salted hash of the IP address (no raw IPs stored) and stores feedback you choose to send. The Glass app itself sends nothing to glassdesk.dev except feedback you submit.

## Requirements

- Host: macOS 13 Ventura or newer (Apple Silicon or Intel), or Linux x86_64 with a Wayland compositor that supports the ScreenCast portal, plus PipeWire.
- Viewer: a browser with WebTransport and WebCodecs, such as Safari 26.4+ on iPadOS/iOS/macOS, or current Chrome, Edge, or Firefox.
- Remote access: Tailscale on both devices with HTTPS Certificates enabled for the tailnet (free plan works).

## Troubleshooting

- **"Glass isn't running":** run `glass status`, then check `~/Library/Logs/Glass/agent.log` and `host.log`.
- **Black screen after connecting:** Glass sends the current frame on connect; if it persists, check Screen Recording permission (`glass status`) and re-run `glass setup`.
- **Mouse and keyboard don't work:** Accessibility permission is missing. Re-run `glass setup` or enable Glass under System Settings → Privacy & Security → Accessibility.
- **After updating, permissions are asked again:** macOS ties permissions to the exact binary. Re-run `glass setup` to re-grant.
- **iPad leaves fullscreen when swiping down:** a Safari fullscreen rule; use Add to Home Screen instead.
- **⌘Tab or ⌘Space doesn't reach the Mac:** iPadOS reserves them; use Ctrl+Tab / Ctrl+Space (Ctrl as ⌘) or the ⌘ menu.
- **"connecting…" forever:** both devices must be on Tailscale, and the URL must use the Mac's `.ts.net` name (not an IP), because the certificate is issued for that name.
- **Displays missing:** sleeping displays aren't capturable; Glass keeps displays awake while running and wakes them on start.

## FAQ

**Is Glass free?** Yes, and open source under the MIT license: https://github.com/Ntarasiuk/glass

**Do I need to install anything on the iPad?** No, just a browser (and Tailscale for remote access).

**Can I use it without Tailscale?** On the Mac itself (localhost), yes. For other devices, Glass needs an HTTPS certificate the browser trusts; Tailscale provides one automatically. Advanced users can pass their own certificate with `--cert`/`--key`.

**How is it different from Chrome Remote Desktop?** Chrome Remote Desktop uses WebRTC's video-call pipeline, which buffers for smoothness. Glass skips that: no jitter buffer, hardware decode straight to canvas, and UDP-style datagrams. Glass also streams multiple displays at once, transfers files, and recovers from iPad tab suspension automatically.

**How is it different from Parsec, Moonlight, or Jump Desktop?** Those need a native app on the viewing device. Glass runs in a browser, aims for similar latency, and is built specifically around a Mac host.

**Does it work on Linux?** Yes, as a viewer in any modern browser, and as a host on Wayland (beta). **Windows hosts?** Not yet.

**Does it stream audio?** Yes, Mac system audio, toggled per device.

**Can I control a locked Mac?** Yes; type your password at the lock screen. FileVault still requires one local login after a reboot.

## Feedback

- From the Mac: `glass feedback "what happened or what you'd like"`
- On the website: https://glassdesk.dev/#feedback
- API: `POST https://glassdesk.dev/api/feedback` with JSON `{ "message": "…", "email": "optional" }`
- GitHub issues: https://github.com/Ntarasiuk/glass/issues
