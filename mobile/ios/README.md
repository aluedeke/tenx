# tenx for iPhone and iPad

The `tenx web` page in a native app (one WKWebView), for what a Home Screen
web app can't do:

- **Microphone, approved once.** iOS asks once for the app; the app grants
  the page's requests itself, on its own server only.
- **Paste without the Paste bubble.** The page asks the app for the
  clipboard (`web/src/lib/native.ts` → the `tenx` message handler). Set
  *Settings → Apps → tenx → Paste from Other Apps → Allow* once; on "Ask",
  iOS hands the app nothing and the page says so.
- **The clipboard in step with the Mac's.** While the app is open, text
  copied on the device lands on the Mac's clipboard within a second, and
  text copied on the Mac on the device's (`web/src/lib/clipsync.ts`; the
  app reads the change count and writes the pasteboard, the page carries
  the text through `/clipboard/get` and `/set`). Concealed copies (password
  managers) stay on the device. Same *Paste from Other Apps → Allow*
  setting; a switch in the app's settings turns it off. Over Tailscale it
  works from anywhere; turn off Handoff on the Mac if Universal Clipboard
  shouldn't move the same text too (the sync ignores an echo either way).
- **No key bar over a hardware keyboard.** The app tells the page when one
  is attached (GameController's `GCKeyboard`; a web page can't know), and
  the page's Esc/Ctrl/paste bar goes away until it is detached.
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

    APP_STORE_CONNECT_API_KEY_ID=… APP_STORE_CONNECT_ISSUER_ID=… ./testflight.sh

App "tenx terminal" in App Store Connect, bundle id `it.luedeke.mobile.tenx`,
team `RUJR5DY35F`. The script signs by hand (why: its header comment) and
needs, once per machine, in `~/.appstoreconnect/`:

- `private_keys/AuthKey_<id>.p8`: an App Store Connect API key (role Admin
  to create certificates and profiles; App Manager is enough to upload);
- `tenx-signing/tenx-signing.keychain-db` holding an "Apple Distribution"
  identity, with the keychain's password in `tenx-signing/kc.pass`;
- the "tenx App Store" provisioning profile (type IOS_APP_STORE, that
  certificate) in `~/Library/Developer/Xcode/UserData/Provisioning Profiles/`.

The certificate and the profile can be made with the API key
(`POST /v1/certificates` with a CSR from a local key, `POST /v1/profiles`).
