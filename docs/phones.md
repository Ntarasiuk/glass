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
(Appium's XCTest server) running on it, forwarded to a port on this computer over USB.

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

   Don't pass `-XCTDisableSnapshottingForEventsOnApplication YES` to make touches about 20 ms
   faster: WebDriverAgent then can't say which app is in front, and Glass keeps agents off
   the phone when it can't tell.

   On iOS 14 and older, Xcode 26 can't build a WebDriverAgent that runs: use a build made with
   Xcode 14 (Appium's v4.x release), re-signed with your team, and mount the developer disk image
   for that iOS version first (`ideviceimagemounter`).
3. Tell Glass about it: `glass config phones xr=8100` (more than one: `xr=8100,se=8101`; the
   picture's port is the command port plus 1000 unless given, e.g. `xr=8100:9200`).

## Over Wi-Fi

A phone paired with Xcode over the network (iOS 17 and later) works without a cable. Turn on
Settings → Developer → Enable UI Automation, run WebDriverAgent with
`xcodebuild test-without-building`, and forward its two ports to this computer's loopback
through Apple's device tunnel (`xcrun devicectl device info details` shows its
`tunnelIPAddress`). Then add `/wifi`: `glass config phones pro=8200:9200/wifi`.

With `/wifi` the picture comes at 30% size and 10 frames a second. The tunnel carries about
6 Mbit/s, and a full-size picture fills it so touches wait seconds behind it; this way they
take about 100 ms.

WebDriverAgent also answers on the phone's own Wi-Fi address, with no sign-in: anyone on the
same network can drive the phone while it runs. Keep such a phone on a network you trust.

## In the viewer

Phones are listed in the Machines menu, under the computer they're plugged into: pick one to
see its live picture and use it with the pointer and keyboard, and pick the computer again to
go back to its screen. A click is a tap, holding is a long press, a drag is a swipe (played back when it's
released: WebDriverAgent plays a gesture as one request), scrolling swipes, and Escape is Home.

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

- Glass only talks to phones on loopback ports. WebDriverAgent has no sign-in, so never expose
  it on a network.
- Phone actions go through the same checks as the computer's: stop and pause, an agent's grant
  and hourly budget, the audit log, and the viewer's activity line.
- For agents under a grant, Glass reads the app in front on the phone before every look or
  touch. Settings, Wallet and Passwords are off limits, an app-limited agent stays in its apps
  (not the home screen), and nothing is typed into a password field.
- Use a phone set aside for agents: one signed into your own Apple ID can reach your messages,
  photos and accounts.
