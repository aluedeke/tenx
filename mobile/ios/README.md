# tenx for iPhone and iPad

The `tenx web` page in a native app (one WKWebView), for what a Home Screen
web app can't do:

- **Microphone, approved once.** iOS asks once for the app; the app grants
  the page's requests itself, on its own server only.
- **Paste without the Paste bubble.** The page asks the app for the
  clipboard (`web/src/lib/native.ts` → the `tenx` message handler). Set
  *Settings → Apps → tenx → Paste from Other Apps → Allow* once; on "Ask",
  iOS hands the app nothing and the page says so.
- **No iPadOS shortcut bar** over a hardware keyboard (`ShortcutBar.swift`;
  it reaches into WebKit's private content view, so a future iOS may bring
  the bar back — it can't crash).
- **The sign-in kept.** The address and token live in the Keychain; every
  launch signs in with `?token=`, so a lost cookie never matters.

Notifications are not in the app yet (a WKWebView has no Web Push); keep the
Home Screen web app for them.

## Use

Start `tenx web` behind `tailscale serve` on your Mac, open the app and paste
the address it prints (`https://<mac>.<tailnet>.ts.net/?token=…`). Settings
come back with a two-finger hold on the page, ⌘, on a keyboard, or *app
settings* in the page's ⋯ menu.

## Build

Open `TenX.xcodeproj` in Xcode (27+), or:

    xcodebuild -project TenX.xcodeproj -scheme TenX -destination 'generic/platform=iOS Simulator' build

The project uses folder-synchronized groups: a file added under `TenX/` is
part of the app without touching the project file. For the simulator, paste
`http://127.0.0.1:7070/?token=…` (http is allowed on this machine only); the
page's own fake server works too: `cd web && node tests/mock-server.mjs`,
then `http://127.0.0.1:7071/?token=x`.

## TestFlight

    ./testflight.sh

with `APP_STORE_CONNECT_API_KEY_ID`, `APP_STORE_CONNECT_ISSUER_ID`,
`APP_STORE_CONNECT_API_KEY_BASE64` (role Admin) and `APPLE_TEAM_ID` in the
environment. Bundle id `it.luedeke.mobile.tenx`.
