# The native viewer

The Glass app shows another computer's screen in its own window (Machines → Connect, or
Machines → Open a link), on a Mac and on Windows. It speaks the web viewer's protocol, so
every host works unchanged: QUIC datagrams over WebTransport on a tailnet or LAN, WebRTC
data channels through glassdesk.dev for links that work from anywhere.

Code: `crates/glass-host/src/viewer/` (session, decoding, platform pieces) and
`crates/glass-host/src/app/remote.rs` (the window).

## Parity with the web viewer

`glass viewer-selftest <test host> --token …` runs the checks named below against a
`glass --test` host, through the same session API the window uses, and reads back what the
host saw. On 2026-11-10 all 28 passed on a Mac and on Windows 11 (Arm VM, x64 build).

| Web viewer | Native viewer | Check |
|---|---|---|
| Video, H.264 | Mac: VideoToolbox into IOSurfaces GPUI draws as they are. Windows: OpenH264 | selftest `connect`; live Mac host from the Mac and from Windows |
| HEVC | Mac: VideoToolbox. Windows: Media Foundation, on the GPU (DXVA) when the decoder takes it, else software. Offered only once a sample frame decodes | `glass viewer-codecs`; `decodes_every_codec` (ignored test, real VideoToolbox); live Mac host from Windows |
| Sharp text, HEVC 4:2:2 | Same rule (Mac: encode-and-decode probe; Windows: a checked-in 4:2:2 frame) | `viewer-probe --sharp` against a live Mac host with `hevc` on, from the Mac and from Windows (host logged "HEVC 4:2:2") |
| Sharp text, lossless pictures | PNG overlays until the screen changes | selftest `sharp text, lossless pictures` |
| Loss repair, keyframe requests, frame acks | Same rules as `video-stream.js` | unit tests `viewer::assemble` |
| Every display, one or all side by side | Display menu; laid out as arranged | selftest `one display`, `all displays side by side` |
| Display previews | Strip along the bottom | selftest `display previews` |
| One window only | Display menu | selftest `one window` |
| Sound (Opus) | cpal output, ~40 ms buffer | selftest `sound` |
| Microphone | cpal input → KIND_MIC | selftest `microphone` |
| Keyboard, system shortcuts | Mac: event tap; Windows: low-level hook | posted key events on the Mac (⌘Tab arrived as 55/48); real injected Win+Tab in the VM |
| Text (IME, accents, emoji) | Input handler → text events | posted ⌥e e on the Mac arrived as "é" |
| ⌘ ↔ Ctrl | Remembered per computer | unit test `keys`; Win+Tab → ⌘Tab in the VM |
| Pointer | Mac: the host's cursor shapes as NSCursors; Windows: drawn by the host | test host |
| Pointer lock (games) | Captured mouse, relative motion | selftest `gamepad + captured mouse`; posted mouse deltas through the tap |
| Game controllers | Mac: GameController; Windows: Windows.Gaming.Input (any pad), XInput fallback | a real DualShock 4 on the Mac and passed through to the VM; unit test `gamepad` |
| Clipboard: text, rich text, images | Both ways, automatically | selftest `clipboard …` (4 checks) |
| Files: send, receive, browser | Drop on the screen; a panel | selftest `file browser (list, fetch)`, `get files from there`; probe upload |
| Terminal | VT100 screen in a panel | selftest `terminal` |
| Chat | Panel; notifications in the background | selftest `chat` |
| Drawing on the screen | Pens, fade, clear | selftest `drawing on the screen` |
| Now playing | Toolbar controls | selftest `media controls` |
| Privacy mode | More menu | selftest `privacy mode` |
| Fit to the window | More menu | selftest `fit to the window` |
| Use as another display | More menu | selftest `use as another display` |
| Record | More menu | selftest `record the session` |
| Restart, update | More menu | selftest `restart Glass there`, `update Glass there` |
| Agents' questions | Cards with Allow / Deny / the number | selftest `an agent's question` (through `glass mcp`) |
| Guest links, passes | Machines → Open a link; passes kept per computer | test host; live Mac host |
| Links with a passkey | The app is its own authenticator, unlocked with Touch ID or Windows Hello | live Mac host: Touch ID (Mac) and Windows Hello PIN (VM), registered then verified |
| Links from anywhere (WebRTC) | Through glassdesk.dev, the answer checked against the link's key | live Mac host |

Not covered by an automatic check: Ctrl+Alt+Delete (needs a Windows host with the
secure-desktop service), Wake-on-LAN (needs a sleeping machine), hardware HEVC decoding on
Windows (needs a GPU with HEVC; the VM's declines it and decodes in software). Touch, Apple
Pencil and trackpad mode don't apply on a Mac or Windows PC.

## More than the browser

- System shortcuts reach the other computer (the browser can't take ⌘Tab, ⌘Space or the Windows key).
- No lag on the pointer: a Mac draws the host's cursor shapes itself.
- No copy between decoder and screen on a Mac.
- Copy and paste with nothing to press.
- Several computers open at once, switched from the sidebar; hidden ones pause their video.
- Settings remembered per computer.

## Testing it

- `glass viewer-selftest <test host> --token …` drives every feature against a `glass --test` host.
- `glass viewer-probe <address or link>` connects without a window (`--input`, `--sharp`, `--click x,y`, `--keys`, `--save shot.png`).
- `glass viewer-codecs`, `glass viewer-gamepad`, `glass viewer-confirm`: what this computer decodes, which controllers it reads, the passkey prompt.
- `GLASS_APP_CONNECT=<address or link>` opens the app straight into a session; `GLASS_APP_PANELS` opens panels.
- `GLASS_VIEWER_RESOLVE=name=ip` reaches a host by name at an address (its certificate still matches).
- `GLASS_VIEWER_DEBUG=1` (or a file path) logs each step of connecting.
