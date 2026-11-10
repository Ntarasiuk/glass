# Phones for agents

An agent using Glass can also use iPhones plugged into that computer. Pass `phone` to
`screenshot`, `click`, `drag`, `scroll`, `type_text`, `press_keys`, `snapshot` and
`wait_for_change`; `list_phones` shows them. `press` and `set_value` take refs from a phone's
`snapshot`.

On a phone:

- `click` is a tap, and `button: "right"` is a long press. `scroll` is a swipe.
- `press_keys` knows `home`, `return`, `backspace`, `tab`, `space`, `volume_up` and `volume_down`.
- `snapshot` reads the screen from the phone's accessibility tree, with refs on what can be tapped.
- There's no pointer (`move_pointer`) and no recording yet.

Glass reaches each phone through [WebDriverAgent](https://github.com/appium/appium-ios/tree/main/packages/webdriveragent)
(Appium's XCTest server) running on it: over USB, or over Wi-Fi through Apple's device
tunnel.

## Set up a phone

1. Plug the iPhone in, unlock it and tap **Trust**. On iOS 16 and later, turn on Developer Mode
   (Settings → Privacy & Security). Set Auto-Lock to Never.
2. Build WebDriverAgent with your Apple Development team, install it and start it, for example
   with [go-ios](https://github.com/danielpaulus/go-ios):

   ```sh
   ios install --path=WebDriverAgentRunner-Runner.app
   ios runwda --bundleid=<your bundle id>.xctrunner --testrunnerbundleid=<your bundle id>.xctrunner --xctestconfig=WebDriverAgentRunner.xctest
   ios forward 8100 8100   # commands
   ios forward 9100 9100   # its live picture, for the viewer
   ```

   That's to try it by hand. With the phone's UDID in step 3, Glass starts WebDriverAgent
   itself and needs no `ios forward`, which opens ports anything on this computer can use.

   Don't pass `-XCTDisableSnapshottingForEventsOnApplication YES` to make touches about 20 ms
   faster: WebDriverAgent then can't say which app is in front, and Glass keeps agents off
   the phone when it can't tell.

   On iOS 14 and older, Xcode 26 can't build a WebDriverAgent that runs: use a build made with
   Xcode 14 (Appium's v4.x release), re-signed with your team, and mount the developer disk image
   for that iOS version first (`ideviceimagemounter`).
3. Tell Glass about it, with its UDID after `@` so Glass keeps it connected:
   `glass config phones xr=8100@00008020-000649323451002E` (more than one:
   `xr=8100@…,se=8101@…`; the picture's port is the command port plus 1000 unless given, e.g.
   `xr=8100:9200@…`). `ios list` or `xcrun devicectl list devices` shows the UDID.

## Glass keeps it connected

With a UDID in its entry, Glass looks after the phone itself; nothing else needs to run:

- It connects to WebDriverAgent's two ports on the phone itself (8100 for commands, 9100 for
  the picture), each time it needs one: over USB through usbmuxd, over Wi-Fi through Apple's
  device tunnel (whose address it looks up again when the phone reconnects). Nothing listens
  on this computer for it, so other programs here and web pages can't reach the phone through
  Glass; the port in its entry just names it.
- It doesn't try to start anything on a phone that isn't there: unplugged, or out of range,
  it's *offline* and says so until it's back.
- It starts WebDriverAgent when it isn't answering, and again whenever it stops (after 5 s,
  doubling to a minute while it keeps failing, and when it hasn't answered 3 minutes after
  starting). With an Xcode build of WebDriverAgent for that
  phone in `<config>/wda/<udid>/` (the `Build/Products` folder of `xcodebuild
  build-for-testing`) it uses Xcode; otherwise go-ios (`ios` on the PATH, or `GLASS_GO_IOS`)
  and the WebDriverAgent installed on the phone.
- It doesn't keep asking a phone that isn't being used: when the iPhone's "allow UI
  automation" prompt goes unanswered, Glass waits until someone locks and unlocks it (or 5
  minutes) before asking again, and doesn't start WebDriverAgent on a locked phone.

The viewer says what each phone needs: *starting…*, *locked*, *unlock it*, *allow
automation*, or *offline* and why, in the Machines menu and over the phone's picture.

Without a UDID, Glass only uses the ports on 127.0.0.1; keeping WebDriverAgent running and
forwarded there is up to you (`ios runwda …` and `ios forward 8100 8100`, `ios forward 9100
9100`), and so is what else can reach those ports (see Safety).

## Over Wi-Fi

A phone paired with Xcode over the network (iOS 17 and later) works without a cable. Turn on
Settings → Developer → Enable UI Automation, put an Xcode build of WebDriverAgent for it in
`<config>/wda/<udid>/`, and add `/wifi`: `glass config phones pro=8200/wifi@<udid>`.

With `/wifi` the picture comes at 30% size and 10 frames a second. The tunnel carries about
6 Mbit/s, and a full-size picture fills it so touches wait seconds behind it; this way they
take about 100 ms.

WebDriverAgent also answers on the phone's own Wi-Fi address, with no sign-in: anyone on the
same network can drive the phone while it runs. Keep such a phone on a network you trust.

## In the viewer

Phones are listed in the Machines menu, under the computer they're plugged into: pick one to
see its live picture and use it with the pointer and keyboard, and pick the computer again to
go back to its screen. A click is a tap, holding is a long press, a drag is a swipe (played back when it's
released: WebDriverAgent plays a gesture as one request), scrolling swipes, a trackpad pinch
pinches, and ⌘V types what's on your clipboard (its first 4 KB). Home is the toolbar's button (Escape isn't
Home: it's also how you leave fullscreen). The pointer is a fingertip, and a ripple shows
where each click landed. Turned on its side, the picture turns upright in the viewer (Glass
asks the phone which way it's held every second) and touches turn with it.

Phones are the owner's: only viewers with full control of the computer see them listed or can
show one. Everyone connected sees the same screen, so a phone can't be shown while a guest, a
view-only or limited link, or another viewer without that access is connected, and a phone
that's shown when one joins is put away first: they keep seeing the computer, never the phone.
If a phone's picture stops (unplugged, WebDriverAgent stopped), viewers are put back on the
computer.

Recordings never include a phone's screen. A recording can't be started while a phone is
shown; one running for a viewer pauses while a phone is shown (its file ends there and the
recording carries on in a new part), and an agent's ends.

While a phone is shown the toolbar is the phone's: Home, App Switcher, Control Center,
Notification Center, Search (Spotlight) and the volume buttons, next to Machines, Keyboard and
Fullscreen. What only a computer has (files, sound, the terminal, Mac shortcuts,
chat, drawing, other displays side by side) is put away until you switch back.

Measured on an iPhone XR (iOS 14.8.1) over USB, six home-screen swipes: from releasing a swipe
to the new picture leaving Glass took 111–166 ms (median about 120 ms), and the viewer showed
it about 13 ms later, plus the network's one-way time. Most of that is the phone: about 10 ms
for the request to reach it, 30–50 ms for WebDriverAgent to put the touch in, and about 100 ms
for the phone to draw it and WebDriverAgent to screenshot it (about 26 a second).

## Safety

- WebDriverAgent has no sign-in: whatever reaches it drives the phone and sees its screen,
  past every check below. A phone Glass manages (a UDID in its entry) is reached directly,
  with no port opened on this computer. Without a UDID, while `ios forward` runs any program
  on this computer can reach WebDriverAgent on 127.0.0.1, and a web page open in one of its
  browsers may be able to send it requests too (pages can send to local ports even when they
  can't read the answers): forward only while you're using the phone, or give it a UDID.
- Even set up over USB, WebDriverAgent also listens on the phone's own Wi-Fi address, where
  anyone on the same network can drive it: keep the phone on a network you trust, or off
  Wi-Fi. Over Wi-Fi pairing, programs on this computer can also reach it at the device
  tunnel's address.
- Phone actions go through the same checks as the computer's: stop and pause, waiting while
  the person uses the computer or a phone from a viewer (looking doesn't wait), an agent's
  grant and hourly budget, the audit log, and the viewer's activity line. An agent's input on
  the computer always goes to the computer, even while a viewer is shown a phone.
- For agents under a grant, Glass reads the app in front on the phone before every look or
  touch. Settings, Wallet and Passwords are off limits, an app-limited agent stays in its apps
  (not the home screen), and while a password field has the keyboard the agent can't type
  there or touch the keyboard and the suggestions above it, though it can tap elsewhere (Sign
  in), scroll and press return. If Glass can't tell whether a password field has the keyboard,
  it doesn't type; if it can't tell where the keyboard is, it doesn't touch the bottom of the
  screen. As on the computer, these limits are for
  agents under a grant; agents you run yourself with `glass mcp`, which act as you, aren't
  limited.
- Use a phone set aside for agents: one signed into your own Apple ID can reach your messages,
  photos and accounts.
