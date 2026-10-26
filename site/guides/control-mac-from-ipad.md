---
path: /guides/control-mac-from-ipad
title: How to control a Mac from an iPad, wirelessly or from afar
label: Control a Mac from an iPad
description: Four ways to control a Mac from an iPad, wirelessly or from anywhere: what Sidecar, Universal Control, Screen Sharing and Glass can and can't do.
order: 1
---

# How to control a Mac from an iPad

**Short answer:** with the iPad next to the Mac, Apple's **Sidecar** extends the Mac's desktop onto the iPad, and on macOS 27 and iPadOS 27 or later you can control it by touch. To control the whole Mac from the iPad anywhere else (another room, another city), you need a remote desktop. **Glass** is a free, open-source one that runs in Safari on the iPad, so there's no app to install, at about 20 ms of delay.

Apple has three built-in features that sound like the answer. Each does something different, and none of them is a remote desktop.

## The options at a glance

| | Sidecar | Universal Control | Screen Sharing | Glass |
|---|---|---|---|---|
| What it does | iPad becomes a second display of the Mac | Mac keyboard and mouse also drive the iPad | Mac to Mac remote control | iPad (any browser) shows and controls the Mac |
| Distance | Within 10 m (30 ft), or a USB cable | Within 10 m | Anywhere the network reaches | Anywhere: your Tailscale network or a one-time link |
| Control the Mac by touch | Limited before macOS 27; expanded on 27 or later | No: it's the other way around | No Apple app on iPad | Yes: touch, trackpad, keyboard, Pencil |
| Apple Pencil | Yes | No | No | Yes, with pressure and tilt |
| Needs | Same Apple Account, Bluetooth, Wi-Fi, Handoff | Same, plus macOS 12.4 and iPadOS 15.4 | Another Mac (or a third-party VNC app) | macOS 13+ on the Mac; Safari 26.4+ on the iPad |
| Price | Free, built in | Free, built in | Free, built in | Free, open source (MIT) |

Sources: Apple's [Sidecar](https://support.apple.com/en-us/102597), [Universal Control](https://support.apple.com/en-us/102459) and [Screen Sharing](https://support.apple.com/guide/mac-help/turn-screen-sharing-on-or-off-mh11848/mac) support pages, checked October 2026.

## Sidecar: the iPad as a second Mac display

Sidecar turns the iPad into an extra display for the Mac, wirelessly within 10 metres or over USB. Both devices need the same Apple Account with two-factor authentication, and Bluetooth, Wi-Fi and Handoff turned on. Apple Pencil works as a pointer and for drawing.

Until macOS 27 and iPadOS 27, touch on a Sidecar screen was limited to a few gestures (scrolling, copy and paste, undo), so you mostly worked it with the Mac's own keyboard and trackpad. Apple says 27 and later add "greatly expanded" touch control of Mac windows and menus. Either way, Sidecar needs the iPad near the Mac, and it shows a second desktop, not the Mac's main screen.

**Choose Sidecar** when the iPad sits next to the Mac on your desk and you want more screen, especially for Pencil work.

## Universal Control: one keyboard and mouse for both

Universal Control lets the Mac's keyboard and trackpad move onto the iPad, with drag and drop between them. Each device keeps its own screen and apps. It doesn't put the Mac on the iPad, and the iPad doesn't control the Mac.

**Choose Universal Control** when both devices are in front of you and you want to type on the iPad with the Mac's keyboard.

## Screen Sharing: Mac to Mac

macOS Screen Sharing controls one Mac from another, and its High Performance mode (Apple silicon and macOS 14 or later on both Macs) is very good on a fast wired network. Apple doesn't make a Screen Sharing app for iPad. An iPad can connect only through a third-party VNC app, and then only in standard VNC mode; Apple warns that VNC viewers may not encrypt what you type.

**Choose Screen Sharing** when you're controlling a Mac from another Mac.

## Glass: the whole Mac on the iPad, from anywhere

[Glass](/) runs on the Mac and streams it to Safari on the iPad:

1. On the Mac, run `curl -fsSL glassdesk.dev | sh`. The Glass app opens and walks you through Tailscale (free) and the two macOS permissions.
2. On the iPad, sign in to Tailscale with the same account, then open the address Glass shows, such as `https://your-mac.your-tailnet.ts.net`.
3. Optional: **Share › Add to Home Screen**, so Glass opens full screen and ⌘ shortcuts go to the Mac.

What you get on the iPad:
- The Mac's own screen, at about 20 ms and up to 120 fps on a ProMotion iPad.
- Touch, trackpad and keyboard with ⌘ shortcuts.
- Apple Pencil pressure and tilt as tablet input in drawing apps.
- The Mac's audio, and your iPad's microphone on the Mac.
- Clipboard and files in both directions.
- Optionally, the iPad as a second display, like Sidecar, from anywhere.

You can also reach the Mac without Tailscale with a one-time link (`glass link --anywhere`), which connects over WebRTC, end-to-end encrypted, with no ports opened. See the [documentation](/docs#connecting).

**Glass's limits:**
- The Mac needs macOS 13 or later.
- The iPad needs Safari 26.4 or later (iPadOS 26.4).
- Glass reaches Macs and Linux computers only; it can't host Windows.
- After the Mac restarts with FileVault, someone has to log in at the Mac once.

## Questions

### Can I control my Mac from my iPad without being near it?

Not with Sidecar or Universal Control, which both need the devices within about 10 metres. You need a remote desktop such as Glass, Jump Desktop or RustDesk. Glass works in Safari, so there's no app to install on the iPad.

### Can I control my Mac from my iPad wirelessly for free?

Yes. Sidecar is free but needs the iPad nearby and gives you a second display, not the Mac's main screen. Glass is free and open source, works from anywhere, and shows the Mac's actual screen.

### Does Apple Pencil work on a Mac from an iPad?

With Sidecar, yes, as a pointer and for drawing. With Glass, Pencil pressure and tilt reach the Mac as tablet input, so drawing apps such as Photoshop, Affinity and Krita respond as they do to a drawing tablet.

### Is there an iPad app for Mac Screen Sharing?

No. Apple's Screen Sharing app is for Macs. On an iPad you need a VNC app or a remote desktop such as Glass.
