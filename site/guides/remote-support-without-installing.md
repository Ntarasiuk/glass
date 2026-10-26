---
path: /guides/remote-support-without-installing
title: Remote support with nothing to install: free tools compared
label: Remote support without installing
description: Free ways to help someone with their computer remotely when they can't install software: Glass, TeamViewer QuickSupport, AnyDesk, Chrome Remote Desktop.
order: 6
---

# Help someone remotely without installing anything

**Short answer:** to help a parent or friend with a Mac or Linux computer, **Glass quick support** has them paste one line into Terminal and read you a nine-digit code. You connect from your browser, and nothing stays on their computer afterwards. For Windows, **TeamViewer QuickSupport** and **AnyDesk** run without installing. **Chrome Remote Desktop**'s support mode is free but installs software on their side.

## Compared

| | Glass quick support | TeamViewer QuickSupport | AnyDesk (portable) | Chrome Remote Desktop | HelpWire |
|---|---|---|---|---|---|
| Their computer | Mac (macOS 13+) or Linux (Wayland, beta) | Windows, Mac | Windows, Mac, Linux | Mac, Windows, Linux | Mac, Windows |
| What they do | Paste one line in Terminal, read a code | Run the QuickSupport program, read an ID | Run AnyDesk without installing, read an address | Install Chrome Remote Desktop, generate a code | Open a link and install the client |
| Left behind | Nothing: it deletes itself | The downloaded program | The downloaded program | The installed host | The installed client |
| You connect from | Any browser | The TeamViewer app | The AnyDesk app | Chrome | The HelpWire app |
| Price | Free, open source | Free for personal use | Free for personal use | Free | Free plan (commercial use allowed) |

Sources: [TeamViewer QuickSupport](https://www.teamviewer.com/en/solutions/use-cases/quicksupport/), [AnyDesk](https://support.anydesk.com/docs/do-i-need-to-install-anydesk), [Chrome Remote Desktop help](https://support.google.com/chrome/answer/1649523), [HelpWire](https://kb.helpwire.app/getting-started/), checked October 2026.

## Glass quick support

1. They open Terminal (on a Mac: ⌘ Space, type Terminal) and paste `curl -fsSL glassdesk.dev/help | sh`.
2. A window shows a nine-digit code. The first time, macOS asks them to allow Screen Recording and Accessibility, and the window has a button for each.
3. You type the code at [glassdesk.dev/help](/help). They're asked before you can see anything.
4. When you're done they click **Stop sharing**, and Glass deletes itself: it ran from a temporary folder, not as a service or at login.

The connection is WebRTC, direct and end-to-end encrypted, with no ports opened. Each code works once, for an hour. You can chat with them and draw on their screen to point at things.

**Limits:** their computer must be a Mac or a Linux PC, and they need to be able to paste into Terminal. Glass doesn't support Windows.

## TeamViewer QuickSupport

A small program they run without installing, which shows an ID and password to read to you. It's free for personal use; helping people commercially needs a license.

## AnyDesk

AnyDesk runs without installing on Windows, macOS and Linux: they open the downloaded file and read you its address. The portable mode has no unattended access and doesn't reconnect after a reboot.

## Chrome Remote Desktop

At remotedesktop.google.com/support they download and install Chrome Remote Desktop, generate a one-time code and approve you. They're asked to confirm again every 30 minutes.

## Questions

### What's the easiest way to help my parents with their Mac remotely?

If they can paste a line into Terminal, Glass quick support needs no account and leaves nothing installed. If they find Terminal daunting, Chrome Remote Desktop or TeamViewer may be easier for them, at the cost of installing software.

### Is it safe to let someone control my computer remotely?

Only with someone you know and trust. Scammers use remote support tools too, so never give a code to someone who contacted you first. With Glass you're asked before anyone sees your screen, the window stays open the whole time, and you can stop at any moment.

### Can I help someone with a Windows PC using Glass?

No. Glass hosts on macOS and Linux only. For Windows, use TeamViewer QuickSupport, AnyDesk or Chrome Remote Desktop.
