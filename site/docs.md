# Glass

> Glass is a free, open-source, low-latency remote desktop for macOS and Linux (Wayland). Install it on the computer you want to control, then see and control it from a browser on an iPad, iPhone, or another computer. Typical latency is 15–30 ms from screen capture to display, over Tailscale or from anywhere through a one-time link (direct WebRTC, end-to-end encrypted). Nothing to install on the viewing device.

- Website: https://glassdesk.dev
- Install (on the computer to be controlled): `curl -fsSL glassdesk.dev | sh`
- Hosts: macOS 13+ (Apple Silicon and Intel), and Linux x86_64 on Wayland with glibc 2.39+ (beta; built for Hyprland/Omarchy, should work on GNOME and KDE; e.g. Arch, Fedora 40+, Ubuntu 24.04+).
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
- **From anywhere.** `glass link --anywhere` makes a one-time link that connects any browser directly to the computer over WebRTC, end-to-end encrypted, with no ports opened. Off until you use it.
- **One-time links.** Let another device in without sharing the token: each link works once and gives that device its own pass, revocable with `glass link --revoke`.
- **Links with limits.** View-only, no files, no clipboard, a time limit, ask the person at the computer first, or require a passkey (Face ID, Touch ID or a security key) every time.
- **Helping someone.** Chat, drawing on the screen (shown on the computer too), a privacy screen that blanks the computer and blocks its own keyboard and mouse, and a connection history.
- **Sharp text.** A per-viewer mode for code and documents: native size, finer frames, HEVC 4:2:2 colour while things move and a lossless full-colour (4:4:4) picture once the screen is still.
- **One window.** Stream a single app's window instead of the whole display (macOS).
- **Terminal and ports.** A shell on the computer inside the viewer, and its local web servers (`/fwd/3000/`) opened from your tailnet.
- **Recording.** Record sessions to MP4 on the computer, per session or always.
- **Controllers and microphone.** Game controllers and your microphone reach the computer (Linux hosts).
- **Keyboards per machine.** ⌘↔Ctrl, PC-keyboard and ⌘/⌥ swaps are remembered for each computer; an optional shortcut bar puts Mac shortcuts one tap away.
- **Power.** Wake a sleeping machine (Wake-on-LAN), restart Glass or the whole computer and reconnect.
- **A console.** `https://<computer>/admin/`: who's connected, requests to allow, links with QR codes, devices, history, settings, access rules and an address book of your machines.
- **For teams.** Single sign-on (any OpenID Connect provider), LDAP, SCIM provisioning, access rules per email, domain or group, and device posture checks.
- **Self-hostable relay.** Run the remote-access introducer and TURN relay yourself.

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

Running the same command again updates Glass in place (it says which version it updated from), restarts the background service, and skips the setup guide if Glass is already set up. Settings and macOS permissions carry over.

## Setup (`glass setup`)

The setup guide walks through four steps and checks each one live:

1. **Tailscale (remote access).** Glass uses [Tailscale](https://tailscale.com) to reach the Mac securely from other devices, with no port forwarding. Setup checks that Tailscale is installed and signed in, and that **HTTPS Certificates** are enabled for your tailnet (one switch on https://login.tailscale.com/admin/dns; browsers require HTTPS for the APIs Glass uses). It then fetches a certificate for the Mac's tailnet name. Without Tailscale, skip this step: setup ends by offering a one-time link that connects your iPad from any network instead (see *From anywhere* below).
2. **Background service.** Glass installs a per-user LaunchAgent (`dev.glassdesk.host`) so it runs in the background, optionally starts at login, and restarts if it crashes.
3. **Screen Recording permission.** macOS asks to let `glass` record the screen. Setup opens the right System Settings pane (Privacy & Security › Screen Recording on macOS 13–14, Screen & System Audio Recording on 15+) and waits until capture works.
4. **Accessibility permission.** Needed to inject mouse and keyboard input. Setup opens the pane and waits until it's allowed.

It finishes by showing the connect URL and a QR code to scan with the iPad.

## Let an AI agent use your computer

`glass mcp` is an [MCP](https://modelcontextprotocol.io) server that gives an AI agent a computer running Glass: its screens, pointer and keyboard. It works on this computer, or on any other one on your tailnet. You watch what it does live from any viewer, and stay in charge:

- A banner shows the agent at work and its recent steps, and a ring marks every click. **Pause** holds its next step until you resume; **Stop** refuses everything until you **Allow again**.
- Before anything hard to undo (sending, buying, deleting, posting) the agent asks you, and a card in the viewer lets you **Allow** or say **Don't**, with an optional note.
- When it's stuck (a sign-in, a 2FA code, a CAPTCHA, a choice only you can make), it asks for help: you do that part from the viewer, tap **Done, hand back**, and it carries on. You also get a notification on the computer.

Add it to Claude Code:

```sh
claude mcp add glass -- glass mcp                      # this computer
claude mcp add mac-mini -- glass mcp --machine mac-mini   # another computer on your tailnet
```

Then ask, for example: "on my Mac, export the frames from the open Figma file to the Desktop". Any MCP client works the same way (command `glass`, argument `mcp`).

Tools: `screenshot`, `list_displays`, `click` (left, right, middle; double and triple), `move_pointer`, `drag`, `scroll`, `type_text` (any text), `press_keys` (`cmd+s`, `return`, `ctrl+c`…), `wait_for_change`, `request_help` and `confirm`. Coordinates are in the pixels of the latest screenshot (1280×800 at most unless the agent asks for more), so they work with computer-use models as is.

Only the computer's owner can drive it: your own tailnet devices, or this computer's token (which `glass mcp` reads by itself on this computer). Guests and links can't. Under the hood it's an HTTP API at `https://<computer>/agent/v1/` (`POST /agent/v1/act` with `{"action": "click", "x": 640, "y": 400}` and so on), if you'd rather call it directly.

## Set up with an AI agent

Glass ships an agent skill: https://glassdesk.dev/skill.md. With it, an agent like Claude Code can set Glass up on any Mac or Linux machine in your Tailscale tailnet over SSH. It finds the machine, installs Glass, runs the unattended setup, tells you exactly which permission switches to click on that machine's screen, waits until everything works, and verifies the connection.

Install it for Claude Code:

```sh
mkdir -p ~/.claude/skills/glass && curl -fsSL https://glassdesk.dev/skill.md -o ~/.claude/skills/glass/SKILL.md
```

Then ask, for example: "set up Glass on my Mac mini". The target needs SSH: Tailscale SSH (`sudo tailscale set --ssh`) or Remote Login on macOS.

The skill drives two commands you can also use in your own scripts:

- `glass setup --yes --json`: does every setup step that needs no human (using `sudo` only if it needs no password), opens the macOS permission panes on the machine's screen, and prints a report: `ok`, `url`, `running`, `capturing`, `input`, and `needs`, the steps left, each with an `action` and whether it must happen `on_screen`.
- `glass status --json`: whether the service is running, capturing, and allowed to inject input.

## Connecting

1. Install Tailscale on the iPad (or other device) and sign in with the same account as the Mac.
2. Open `https://<your-mac>.<your-tailnet>.ts.net` (shown at the end of setup, and by `glass status`). The same address with `:8080` also works.
3. Optional: in Safari or Chrome, tap **Share → Add to Home Screen**, then launch Glass from its icon. It runs fullscreen with no browser UI, so Safari's own shortcuts (⌘W, ⌘T, ⌘L, ⌘R) reach the Mac instead.

Devices that aren't on your tailnet, or belong to another tailnet user, are asked for the access token (`glass token` shows it; `glass token --set` lets you choose one). A link containing `#t=<token>` signs a browser in once and is then remembered.

**Magic links.** To let in a device without handing out the token, run `glass link` on the host; it prints the link and a QR code. The link works once, within 15 minutes, and gives that device its own access for 24 hours by default (`glass link --for 7d` to change it). Guests can use the computer but can't switch machines or update Glass. `glass link --revoke` cuts every guest off within seconds and voids unused links; your token and tailnet devices are unaffected. Only fingerprints of links and passes are stored, in `~/.config/glass/access.json`. On your tailnet, the guest's device must be on the tailnet too; for anywhere else, see the next paragraph.

**From anywhere (no Tailscale on the device).** `glass link --anywhere` makes a link like `https://glassdesk.dev/app/#r=…&l=…` that works from any network. The browser and your computer connect directly over WebRTC: UDP, end-to-end encrypted, about as fast as on the tailnet. When a direct path is impossible (some cellular and corporate networks), traffic goes through Cloudflare's TURN relay, still end-to-end encrypted. glassdesk.dev only introduces the two sides; your computer keeps one outgoing connection to it and opens no ports. The link carries your computer's public key and the computer signs every connection answer, so the browser refuses anyone else, glassdesk.dev included. Remote access is off until you make such a link (or run `glass remote on`); `glass remote off` turns it off. One thing to know: in this mode the viewer page itself comes from glassdesk.dev, whereas on your tailnet it comes from your own computer.

## Links with limits

`glass link` takes options that limit what the device may do:

- `--view-only`: watch only; no mouse, keyboard or controllers.
- `--no-files`, `--no-clipboard`: no file transfer, no copy and paste.
- `--for 1h`: how long the device's pass lasts (default 24 hours).
- `--ask`: the person at the computer allows each connection, from a notification or with `glass allow` / `glass deny`. `glass config ask-guests on` asks for every guest; `ask-everyone on` asks for everyone.
- `--passkey`: the device registers a passkey on first use and must use it every time, so a copied link is useless on another device.

`glass link --list` shows the devices let in by links, each with an id; `glass link --revoke <id>` cuts off one. `glass viewers` shows who's connected now and `glass kick [id]` ends a session. `glass log` is the connection history: who, when, how, and for how long.

## Using Glass

The toolbar (top right) has:

- **Displays:** pick one display, or **All displays** to show every screen side by side in the same arrangement as System Settings → Displays.
- **⌘ (Mac shortcuts):** one-tap app switcher, Spotlight, Mission Control, Hide, Quit, Close window, screenshot, Force Quit, and Lock Screen. Also keyboard remapping options (see below).
- **Keyboard:** brings up the iPad's on-screen keyboard.
- **Send files to Mac:** uploads to `~/Downloads` (drag and drop also works).
- **Get files from Mac:** opens a file chooser on the Mac's screen; chosen files arrive with a Save button.
- **Audio:** turns Mac system audio on or off for this device.
- **Fullscreen** (hidden when running as a Home Screen app) and **Stats**.
- **Hide toolbar** (last button): slides it away for an unobstructed screen; the small tab at the top brings it back. Remembered.

**Display previews.** With two or more displays, a strip of small live previews of each one (and **All**) sits in the empty band below the picture whenever there's room; the one being shown is outlined, and tapping another switches to it. Without room it waits behind a small tab at the bottom. Hide it with its ⌄ button; that's remembered. Live previews of displays not being shown need macOS 14 or newer.

Touch gestures: tap = click, drag = drag, long-press = right-click, two fingers = scroll.

More in the toolbar:

- **Displays › Sharp text:** for code and documents. See *Sharp text* below.
- **Displays › Apps:** show one app's window on its own (macOS).
- **Draw on the screen:** annotate; the drawing also shows on the computer's screen.
- **Chat:** messages with the person at the computer (`glass chat "…"` from there).
- **Terminal:** a shell on the computer, in the viewer. Drag its top edge to resize it, or use **Full screen** (or double-click its title bar); both are remembered.
- **Forwarded ports:** open the computer's local web servers (only ports allowed with `glass config forward-ports 3000,8888`, and only for your own devices).
- **Record on the host:** records this session to `~/Movies/Glass` (`~/Videos/Glass` on Linux). `glass config record on` records every session.
- **Microphone:** sends your microphone to the computer (Linux hosts).
- **Computer:** **Privacy screen** (blanks the computer's screen and ignores its keyboard and mouse while you're connected; `glass privacy on|off` from there), **Restart Glass**, **Restart computer** (Glass reconnects once someone is logged in) and **Console**.
- **Machines:** sleeping machines Glass has seen before can be woken with Wake-on-LAN (also `glass wake <name|mac>`).

Game controllers connected to the viewing device work on Linux hosts as a virtual Xbox 360 controller.

Local shortcuts (not sent to the Mac): Ctrl+Shift+F toggles fullscreen, Ctrl+Shift+H toggles the stats overlay.

### Keyboard and ⌘ shortcuts on iPad

iPadOS reserves a few shortcuts before any app or web page sees them: ⌘Tab, ⌘Space, ⌘H, ⌘⇧3/4/5, and Globe-key combinations. Glass offers three ways around this:

- **Ctrl as ⌘** (on by default): Ctrl+Tab, Ctrl+Space, Ctrl+H, and Ctrl+⇧3/4/5 are sent to the Mac as their ⌘ versions. Holding Ctrl and tapping Tab cycles apps exactly like ⌘Tab. Every other Ctrl combination stays Ctrl.
- **Swap ⌘ and ⌥:** pair with iPad Settings → General → Keyboard → Hardware Keyboard → Modifier Keys (Command → Option, Option → Command). The physical ⌘ key then reaches the Mac as ⌘ even for reserved shortcuts.
- **⌘ menu:** one-tap buttons for the reserved shortcuts.

These choices are remembered for each computer. **Shortcut bar** (⌘ menu) adds a row of Mac shortcuts above the screen on touch devices.

Everything else, including ⌘C, ⌘V, ⌘Q, ⌘W, and arrows, works directly from a hardware keyboard.

### Clipboard

- ⌘C or ⌘X on the Mac (from the iPad keyboard) puts the Mac's copied text on the iPad's clipboard.
- ⌘V pastes the iPad's clipboard into the Mac.
- Any Mac clipboard change is sent to connected devices.

### Sharp text

Turn on **Displays › Sharp text** when reading code or documents. The display streams at its native size with fewer but finer frames, in HEVC 4:2:2 colour where the Mac's encoder and the browser allow it, so coloured text keeps its edges. Once the screen stops changing, Glass sends a lossless full-colour (4:4:4) picture of it, so still text is pixel-exact; the next change resumes video. It's per viewer and remembered per computer. Elsewhere, `glass config hevc on` uses HEVC for everyone when every connected viewer can decode it.

## Console

`https://<computer>.<tailnet>.ts.net/admin/` (or **Computer › Console** in the viewer) is the owner's console, open to your own tailnet devices and the access token. It shows who's connected (and ends sessions), connection requests to allow or deny, one-time links with QR codes and limits, devices let in by links, connection history, settings, access rules, and an address book: your machines in groups you name, each opening in the viewer.

## Teams: sign-in, access rules, posture

**Single sign-on.** Register Glass with any OpenID Connect provider (Google, Microsoft Entra ID, Okta, Auth0, Keycloak…) with redirect URL `https://<computer>/sso/callback`, then:

```sh
glass config oidc-issuer https://accounts.google.com
glass config oidc-client-id <id>
glass config oidc-client-secret <secret>
```

The viewer then offers **Sign in with your organization** instead of asking for a token.

**LDAP.** `glass config ldap-url ldaps://dir.example.com` (or `ldap://` with StartTLS), then either `ldap-user-dn uid={user},ou=people,dc=example,dc=com` to bind directly, or `ldap-base-dn` plus optional `ldap-bind-dn` / `ldap-bind-password` to search for the user. Groups come from `memberOf` (`ldap-group-attr` to change). The viewer asks for a directory username and password.

**Access rules.** Who may sign in, and what they may do, first match wins:

```sh
glass acl add group:eng --view-only --for 8h
glass acl add @example.com
glass acl add ana@example.com --ask
glass acl                 # list
glass acl remove 0        # by position
```

A rule matches an email, `@domain`, `group:name` or `*`, and takes the same limits as `glass link`. With no rules, nobody signs in with a directory account. Rules live in `~/.config/glass/acl.json`.

**SCIM.** `glass config scim-token new` prints a token for SCIM 2.0 provisioning at `https://<computer>/scim/v2` (Okta, Entra ID). Users and groups stay in sync; deactivated users can't sign in and lose their sessions at once.

**Device posture.** Checked on every connection:

- `posture-tailnet-tags tag:trusted`: tailnet devices must carry one of these Tailscale tags.
- `posture-os macos,ios`: only these operating systems.
- `posture-min-browser 120`: oldest browser major version let in.
- `posture-require-passkey-uv on`: passkey links need Face ID, Touch ID or a PIN, not just a tap.

## Branding and self-hosting

`glass config name "Studio"` and `glass config accent "#ff8800"` change the name and colour shown in the viewer. To run remote access without glassdesk.dev, deploy the introducer and TURN relay from [site/selfhost](https://github.com/Ntarasiuk/glass/tree/main/site/selfhost) and point Glass at it with `glass config relay https://glass.example.com`.

## CLI reference

```
glass               Run the host in the foreground (the background service runs this)
glass setup         Guided setup: Tailscale, background service, permissions, connect
glass setup --yes [--json]   Unattended setup for scripts and agents; reports what's left
glass status [--json]        Is the service running? Capturing? Permissions? Connect URL
glass link [--for 24h]   One-time magic link (and QR code) for a device outside your tailnet
glass link --anywhere    The same, working from any network (WebRTC via glassdesk.dev)
glass link --view-only --no-files --no-clipboard --ask --passkey   Links with limits
glass link --list        Devices let in with links, with ids
glass link --revoke [id] Cut off one device, or every device let in with a magic link
glass viewers [--json]   Who's connected now
glass kick [id]          End one session, or every one
glass requests / allow [id] / deny [id]   Connection requests waiting for an answer
glass chat "…"           Message everyone viewing
glass privacy on|off     Blank this screen and block local input while viewed
glass log [-n 20] [--json]   Connection history
glass wake <name|mac>    Wake a machine with Wake-on-LAN
glass acl [add|remove]   Access rules for SSO and LDAP accounts
glass config [key [value]]   Settings (no arguments: list them all with descriptions)
glass remote on|off      Remote access from outside your tailnet (status without an argument)
glass token         Show the access token for devices outside your tailnet
glass token --reset Replace it with a new random token
glass token --set   Choose your own token (prompted; or piped: echo … | glass token --set)
glass mcp [--machine NAME]   MCP server letting an AI agent use this computer (or NAME on your tailnet)
glass feedback "…"  Send feedback to the developers
glass uninstall     Stop the background service and remove it from login items
glass --version
```

Host options: `--http-port` (default 8080; if it's taken, the next free port up to 8099), `--https-port` (default 443: also serve there when free, so URLs need no port; 0 turns it off), `--wt-port` (UDP, default 4433; likewise moves to the next free port, which browsers learn automatically), `--fps` (default 60), `--mbps` (default 30), `--scale`, `--cert`/`--key` (custom TLS), `--token-file`, `--web-dir`, `--log-dir`.

Files:

- `~/.glass`: installed binary and web client
- `~/.config/glass/token`: access token (created on first run)
- `~/.config/glass/access.json`: magic links and guest passes (fingerprints only)
- `~/.config/glass/settings.json`: settings from `glass config`
- `~/.config/glass/acl.json`: access rules
- `~/.config/glass/connections.jsonl`: connection history
- `~/.config/glass/oidc-secret`, `ldap-bind-password`, `scim-token`: secrets, owner-only
- `~/Movies/Glass/` (`~/Videos/Glass/` on Linux): recordings
- `~/.config/glass/certs/`: Tailscale HTTPS certificate
- `~/.config/glass/status.json`: live service status
- `~/Library/Logs/Glass/`: host, client, and agent logs
- `~/Library/LaunchAgents/dev.glassdesk.host.plist`: background service

## How it works

- **Capture:** ScreenCaptureKit, zero-copy IOSurface frames, only when the screen changes. A static screen costs nothing.
- **Encode:** VideoToolbox hardware H.264 (HEVC when enabled and every viewer can decode it; 4:2:2 for Sharp text) in low-latency rate-control mode, with no B-frames. Keyframes are sent on demand (new viewer or detected loss) and at least every 900 frames.
- **Transport:** WebTransport (HTTP/3 over QUIC). Video and audio travel as unreliable datagrams, so a lost packet never blocks newer ones. Input, control, clipboard, and files use reliable streams.
- **Decode and render:** WebCodecs hardware decoding in a Web Worker, drawn to an OffscreenCanvas. The page's main thread only handles input.
- **Audio:** 48 kHz PCM in 5 ms packets, played through an AudioWorklet with a ~40 ms jitter buffer.
- **Integrity and recovery:** each frame carries a checksum; corrupted or incomplete frames are never decoded. The client requests a keyframe and recovers within a fraction of a second. If the browser's hardware decoder misbehaves, Glass falls back to more conservative decoder settings and remembers the choice.
- **Lossless refresh:** in Sharp text, once the screen is still, a PNG of the full frame (full 4:4:4 colour) is drawn over the video until the next change.
- **Clock sync:** NTP-style ping/pong over datagrams, used for the live latency display.

## Linux host (beta)

The same one-liner installs Glass on Linux x86_64. The Linux host uses:

- **Screen capture:** xdg-desktop-portal ScreenCast over PipeWire. On Hyprland (Omarchy), the share picker asks once which screen to share; Glass saves a restore token so it doesn't ask again.
- **Encoding:** software H.264 (OpenH264, screen-content mode). Screens larger than 2560×1600 are halved before encoding, to keep the frame rate up. Hardware VA-API encoding is planned.
- **Input:** a virtual keyboard and absolute pointer through `/dev/uinput`. `glass setup` installs a one-line udev rule (`/etc/udev/rules.d/60-glass-uinput.rules`) giving the logged-in user access; this needs `sudo` once.
- **Address:** Linux only lets root listen on port 443, so `glass setup` runs `tailscale serve` to forward `https://<machine>.<tailnet>.ts.net/` to Glass. If that isn't possible, use the `:8080` address.
- **Clipboard:** `wl-copy` / `wl-paste` (wl-clipboard).
- **Audio:** the default output's monitor, through PipeWire. The viewer's microphone appears as a "Glass microphone" input while it's on.
- **Controllers:** game controllers on the viewing device appear as a virtual Xbox 360 controller through `/dev/uinput`.
- **Sharp text:** the lossless full-colour refresh works on Linux; HEVC 4:2:2 is macOS only.
- **Background service:** a systemd user unit (`~/.config/systemd/user/glass.service`) tied to the graphical session. Logs: `journalctl --user -u glass` and `~/.local/state/glass/`.
- **Tailscale:** non-root users need to be Tailscale operators to fetch HTTPS certificates; setup offers to run `sudo tailscale set --operator=$USER`.

Dependencies: PipeWire, wl-clipboard and your desktop's screen-sharing portal (`xdg-desktop-portal-hyprland`, `-gnome`, `-kde` or `-wlr`). Setup detects pacman, apt, dnf or zypper and offers to install what's missing, e.g. on Arch / Omarchy: `sudo pacman -S --needed pipewire xdg-desktop-portal-hyprland wl-clipboard`; on Ubuntu: `sudo apt-get install pipewire xdg-desktop-portal-gnome wl-clipboard`. Tailscale: `curl -fsSL https://tailscale.com/install.sh | sh`, then `sudo tailscale up`.

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
- Access control: a device is let in if Tailscale reports it belongs to the same user as the Mac, or if it presents the access token. Glass sends nothing to a session (video, audio, clipboard, input) until it is authenticated. Links, single sign-on, LDAP, access rules, posture checks and passkeys can narrow that further; see above.
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
- **After updating, permissions are asked again:** releases are signed with the same identity, so macOS keeps Screen Recording and Accessibility across updates. Glass you built yourself, or a copy you run from a terminal, is a different app to macOS and needs its own grant. Re-run `glass setup` to grant it.
- **iPad leaves fullscreen when swiping down:** a Safari fullscreen rule; use Add to Home Screen instead.
- **⌘Tab or ⌘Space doesn't reach the Mac:** iPadOS reserves them; use Ctrl+Tab / Ctrl+Space (Ctrl as ⌘) or the ⌘ menu.
- **"connecting…" forever:** both devices must be on Tailscale, and the URL must use the Mac's `.ts.net` name (not an IP), because the certificate is issued for that name.
- **Displays missing:** sleeping displays aren't capturable; Glass keeps displays awake while running and wakes them on start.

## FAQ

**Is Glass free?** Yes, and open source under the MIT license: https://github.com/Ntarasiuk/glass

**Do I need to install anything on the iPad?** No, just a browser. Tailscale is optional: a one-time link from `glass link --anywhere` works without it.

**Can I use it without Tailscale?** Yes. `glass link --anywhere` makes a one-time link that connects any browser directly over WebRTC, end-to-end encrypted, with no open ports. On a tailnet, Glass uses WebTransport with a certificate Tailscale provides; advanced users can pass their own with `--cert`/`--key`.

**How is it different from Chrome Remote Desktop?** Chrome Remote Desktop uses a video-call pipeline tuned for smoothness. Glass is tuned for latency: on a tailnet it skips WebRTC entirely (UDP-style WebTransport datagrams, hardware decode straight to canvas, no jitter buffer), and from anywhere it uses WebRTC with the smallest jitter buffer the browser allows. Glass also streams multiple displays at once, transfers files, and recovers from iPad tab suspension automatically.

**How is it different from Parsec, Moonlight, or Jump Desktop?** Those need a native app on the viewing device. Glass runs in a browser, aims for similar latency, and is built for Mac and Linux hosts.

**Does it work on Linux?** Yes, as a viewer in any modern browser, and as a host on Wayland (beta).

**Does it work on Windows?** As a viewer, yes, in any modern browser. Windows can't be the computer you control yet.

**Does it stream audio?** Yes, the host's system audio, toggled per device.

**Can I control a locked Mac?** Yes; type your password at the lock screen. FileVault still requires one local login after a reboot.

## Feedback

- From the Mac: `glass feedback "what happened or what you'd like"`
- On the website: https://glassdesk.dev/features#feedback
- API: `POST https://glassdesk.dev/api/feedback` with JSON `{ "message": "…", "email": "optional" }`
- GitHub issues: https://github.com/Ntarasiuk/glass/issues
