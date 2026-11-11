# The native viewer

The Glass app shows another computer's screen in its own window (Machines → Connect, or
Machines → Open a link), on a Mac and on Windows. It speaks the web viewer's protocol, so
every host works unchanged: QUIC datagrams over WebTransport on a tailnet or LAN, WebRTC
data channels through glassdesk.dev for links that work from anywhere.

Code: `crates/glass-host/src/viewer/` (session, decoding, platform pieces) and
`crates/glass-host/src/app/remote.rs` (the window).

## Parity with the web viewer

| Web viewer | Native viewer | How it was checked |
|---|---|---|
| Video, H.264 | Mac: VideoToolbox into IOSurfaces GPUI draws as they are. Windows: OpenH264 | Test host, live Mac host, Windows VM |
| HEVC | Mac: VideoToolbox. Windows: Media Foundation on the GPU (DXVA) where it can, else software. Offered only once a sample frame decodes | Test vectors on both; live Mac host from the VM |
| Sharp text, HEVC 4:2:2 | Same rule (Mac: encode-and-decode probe; Windows: checked-in 4:2:2 frame) | Live Mac host with `hevc` on, from the Mac and the VM |
| Sharp text, lossless pictures | PNG overlays until the screen changes | Test host |
| Loss repair (NACK), keyframe requests, frame acks | Same rules as `video-stream.js` | Unit tests; test host |
| Every display, one at a time or side by side | Display menu; layout as the displays are arranged | Test host (2 displays) |
| Display previews | Strip along the bottom | Test host |
| One window only | Display menu | Code path; host message |
| Sound (Opus) | cpal output, ~40 ms buffer | Self-test (muted), Mac and Windows |
| Microphone | cpal input → KIND_MIC | Self-test, Mac and Windows |
| Keyboard | Mac: event tap; Windows: low-level hook. System shortcuts go through (⌘Tab, ⌘Space, Win, Alt+Tab) | Synthetic events on the Mac; real injected keys in the VM |
| Text (IME, accents, emoji) | Input handler → text events | Dead-key é on the Mac |
| ⌘ ↔ Ctrl per machine | Remembered per computer | Code path |
| Pointer | Mac: the host's cursor shapes as NSCursors (no lag); Windows: drawn by the host | Test host |
| Pointer lock (games) | Captured mouse, relative motion | Synthetic events on the Mac |
| Game controllers | Mac: GameController; Windows: Windows.Gaming.Input (any pad), XInput fallback | A real DualShock 4 on the Mac and passed through to the VM |
| Clipboard: text, rich text, images | Both ways, automatically | Self-test, Mac and Windows |
| Files: send, receive, file browser | Drop on the screen; Downloads; a browser panel | Test host |
| Terminal | VT100 screen in a panel | Self-test (`echo glass-$((40+2))`) |
| Chat | Panel; notifications in the background | Self-test |
| Drawing on the screen | Pens, fade, clear | Code path; screenshots |
| Now playing | Toolbar controls | Test host |
| Privacy mode, fit, use as a display, record, restart, update, Ctrl+Alt+Del | More menu | Code path; host messages |
| Agents' questions | Cards with Allow / Deny / the number | Code path |
| Guest links, passes | Opened from Machines; passes kept per computer | Test host; live Mac host |
| Links with a passkey | The app is its own authenticator: a P-256 key unlocked with Touch ID or Windows Hello | Touch ID and Windows Hello prompts against the live Mac host |
| Links from anywhere (WebRTC) | Through glassdesk.dev, the answer checked against the link's key | Live Mac host |
| Notifications (Web Push) | Native notifications | Code path |
| Touch, Apple Pencil, trackpad mode | Not applicable on a Mac or Windows PC | — |

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
