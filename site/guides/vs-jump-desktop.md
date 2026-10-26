---
path: /vs/jump-desktop
title: Glass vs Jump Desktop: a Mac from an iPad, compared
label: Glass vs Jump Desktop
description: Glass is free and runs in the browser; Jump Desktop is a polished paid app that also reaches Windows. Platforms, price, latency, Pencil and more.
order: 10
---

# Glass vs Jump Desktop

**Short answer:** **Jump Desktop** is the long-standing, polished remote desktop app for iPad and Mac. It reaches Mac and Windows computers (Linux in alpha) and speaks its own Fluid protocol as well as RDP and VNC. The Mac and iPad apps are one-time purchases. **Glass** is free and open source and needs no app: you open your Mac or Linux computer in Safari or any browser. It adds Apple Pencil pressure and the iPad as a second display for the Mac. Choose Jump Desktop if you need Windows or RDP; choose Glass for a free, browser-only way to your own Mac or Linux machine.

## Compared

| | Glass | Jump Desktop |
|---|---|---|
| Price | Free, open source (MIT) | Mac app $34.99 and iPad/iPhone app $14.99, one-time; free on Windows, Android and the web |
| Computers you control | macOS 13+, Linux on Wayland (beta) | macOS 10.15+, Windows; Linux in alpha (Wayland or X11) |
| What you control it from | Any modern browser | Apps for Mac, Windows, iPad/iPhone, Android (beta), and a web client |
| Protocols | Its own: hardware H.264/HEVC over WebTransport or WebRTC | Fluid (its own), RDP, VNC |
| Speed | About 15–30 ms measured in-app; up to 120 fps | Up to 480 fps claimed, 120 fps on ProMotion iPads; no latency figure published |
| Apple Pencil | Pressure and tilt, as tablet input on the Mac | Works as a mouse |
| iPad as a second display for the Mac | Yes, extends the Mac's desktop | Virtual displays for the remote session; no Sidecar-style extend |
| Account | None: your Tailscale network, or one-time links | Sign in to set up a computer; "Cloudless Fluid" by hostname or IP |
| Helping someone | One line in Terminal and a code; nothing left installed | They install Jump Desktop Connect; you can join from a browser |
| AI agents | Built-in MCP server; you watch and approve | Not offered |

Sources: [Jump Desktop](https://jumpdesktop.com/), [pricing](https://jumpdesktop.com/pricing-plans.html), [supported hosts](https://docs.jumpdesktop.com/connect/install/), [iPad features](https://jumpdesktop.com/remote-desktop-iphone-ipad.html), [virtual displays](https://docs.jumpdesktop.com/viewer/virtual-displays/), App Store listings, checked October 2026.

## Where Jump Desktop is the better choice

- **Windows computers**, and RDP or VNC servers you already run.
- **Very high refresh rates**, with virtual displays at 120, 144 or 240 Hz for the remote session.
- **A native app on every device**, with a decade of polish on iPad keyboard and mouse handling.
- **Teams**, with Jump Desktop's managed plans for IT.

## Where Glass is the better choice

- **Free and open source.** No purchase on the Mac or the iPad, and the code is on [GitHub](https://github.com/Ntarasiuk/glass).
- **Nothing to install on the iPad.** Safari is the client. Add it to the Home Screen and it runs full screen.
- **Apple Pencil as a real pen.** Pressure and tilt reach the Mac as tablet input, so drawing apps respond as they do to a Wacom tablet.
- **The iPad as a second display** that extends the Mac's desktop, like Sidecar, but from anywhere.
- **Linux on Wayland**, Hyprland included, through the screen-sharing portal.
- **Quick support** that leaves nothing on the other person's computer.
- **AI agents** that use the computer while you watch, pause and approve.

## Questions

### Is there a free alternative to Jump Desktop for iPad?

Yes. Glass is free and open source, and runs in Safari on the iPad, so there's no app to buy. It reaches Mac and Linux computers; for Windows, consider RustDesk or Chrome Remote Desktop.

### Which has less latency?

Glass measures and shows its latency live: about 15–30 ms on a home network. Jump Desktop publishes frame-rate figures (up to 480 fps) but no latency figure, so they're hard to compare without testing both on your own network.

### Does Jump Desktop work with Apple Pencil?

Jump Desktop treats the Pencil as a mouse. Glass passes Pencil pressure and tilt to the Mac as tablet input.
