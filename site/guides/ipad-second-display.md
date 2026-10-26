---
path: /guides/ipad-second-display
title: iPad as a second Mac display: Sidecar, Luna, Duet, Glass
label: iPad as a second display
description: How to use an iPad as a second monitor for a Mac: Sidecar, Luna Display, Duet Display and Glass compared on price, range, wired or wireless.
order: 4
---

# Use an iPad as a second display for a Mac

**Short answer:** if your iPad and Mac support it and the iPad is within 10 metres, use **Sidecar**: it's built in and free. **Luna Display** ($89.99 dongle) and **Duet Display** (subscription) add older or non-Apple setups and Windows. **Glass** does it free from any browser, so the second display can also be a laptop or an iPad in another room, and it doubles as a full remote desktop.

## Compared

| | Sidecar | Luna Display | Duet Display | Glass |
|---|---|---|---|---|
| Price | Free, built in | $89.99, one-time (a USB-C dongle) | Free app; $3.99–$79.99 subscriptions | Free, open source |
| Connection | Wireless within 10 m, or USB | Wi-Fi or wired | Software only | Wi-Fi, Tailscale or a one-time link |
| Computers | Mac | Mac or Windows | Mac or Windows | Mac (second display); Mac and Linux for remote control |
| Displays | iPad | iPad or Mac | iPad, iPhone, Android | Any browser: iPad, laptop, phone |
| Pencil | Yes | Yes | Varies by plan | Yes, with pressure and tilt |

Sources: [Apple Sidecar](https://support.apple.com/en-us/102597), [Luna Display](https://astropad.com/product/lunadisplay/), [Duet Display on the App Store](https://apps.apple.com/us/app/duet-display/id935754064), checked October 2026.

## Sidecar

On the Mac, open **Control Center › Screen Mirroring** (or the Displays settings) and choose the iPad.

Requirements:
- macOS 10.15 or later and iPadOS 13 or later, on supported models
- the same Apple Account with two-factor authentication
- Bluetooth, Wi-Fi and Handoff turned on
- within 10 metres, or a USB cable

Switching to an iPad app pauses Sidecar.

## Luna Display

A hardware dongle in the Mac's USB-C port. Astropad quotes 7–25 ms over Wi-Fi and 1–4 ms wired. It works with older Macs (2011 and later) that Sidecar doesn't support, and with Windows.

## Duet Display

Software only, with a free download and paid tiers for the features you need. It works on Mac and Windows computers, with iPad, iPhone and Android as displays.

## Glass

In the Glass viewer, choose **Displays › Use as a second display**. The browser you're in becomes one more display of the Mac, to the right of its main one, in Retina on an iPad, and the desktop extends onto it. Drag windows onto it from the Mac. It goes away when you disconnect.

Because the display is just a browser, it can be:
- an iPad or a laptop of any brand, on the same Wi-Fi
- an iPad across town, over your Tailscale network or a one-time link

The same connection also gives you the Mac's main screen, audio and input when you want to control it instead.

**Glass's limits:**
- The second display needs a Mac with macOS 13 or later.
- Everyone viewing sees the same display, so it's best used alone.
- Sidecar over USB has less delay than any Wi-Fi option.

## Questions

### Can I use my iPad as a second monitor without Sidecar?

Yes. Glass is free and works from Safari. Duet Display and Luna Display are paid alternatives that also support Windows.

### Can I use an iPad as a second display for a Mac over the internet?

Sidecar needs the iPad within 10 metres. Glass works over your Tailscale network or a one-time link, so the iPad can be anywhere.

### Which is fastest?

A cable. Sidecar over USB or Luna Display wired (1–4 ms by Astropad's figures) beats Wi-Fi. Over Wi-Fi, Glass measures about 15–30 ms on a home network.
