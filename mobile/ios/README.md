# tenx for iPhone and iPad

The `tenx web` page in a native app (one WKWebView), for what a Home Screen
web app can't do:

- **Microphone, approved once.** iOS asks once for the app; the app grants
  the page's requests itself, on its own server only.
- **Paste without the Paste bubble.** The page asks the app for the
  clipboard (`web/src/lib/native.ts` → the `tenx` message handler). Set
  *Settings → Apps → tenx → Paste from Other Apps → Allow* once; on "Ask",
  iOS hands the app nothing and the page says so.
- **The clipboard in step with the Mac's.** While the app is open, text or
  an image copied on the device lands on the Mac's clipboard within a
  second, and what is copied on the Mac on the device's
  (`web/src/lib/clipsync.ts`; the app reads the change count and writes the
  pasteboard, the page carries the copy through `/clipboard/get`, `/image`
  and `/set`). An image goes over when the copy has no text of its own
  (a screenshot, a photo, an image file copied in Finder), up to 25 MB;
  HEIC and TIFF arrive as PNG. Concealed copies (password managers) stay on
  the device. Same *Paste from Other Apps → Allow* setting; a switch in the
  app's settings turns it off. Over Tailscale it works from anywhere; turn
  off Handoff on the Mac if Universal Clipboard shouldn't move the same
  copy too (the sync ignores an echo either way).
- **iOS's own menu for a link in the terminal.** Hold a finger on a link
  and lift it: Copy, Open, Share at the finger (`LinkMenu.swift`, asked for
  by the page through `linkMenu`). The terminal's links are text it draws,
  not anchors, so WebKit has no menu for them; in Safari, or an app too old
  for `linkMenu`, the page shows its own.
- **No key bar over a hardware keyboard.** The app tells the page when one
  is attached (GameController's `GCKeyboard`; a web page can't know), and
  the page's Esc/Ctrl/paste bar goes away until it is detached.
- **No iPadOS shortcut bar** over a hardware keyboard (`ShortcutBar.swift`;
  it reaches into WebKit's private content view, so a future iOS may bring
  the bar back — it can't crash).
- **The window buttons free.** In a window on iPadOS 26 and later, the
  close/minimize/full-screen dots sit in the top-left corner; the app
  measures them and the page's header starts to their right
  (`--window-controls`), so a tap on them reaches them, not the page.
- **Reload Page (⌘R)** in the Window menu of the iPad's menu bar: a new web
  view, so a newer `tenx web` page shows without quitting the app.
- **The sign-in kept.** The address and token live in the Keychain; every
  launch signs in with `?token=`, so a lost cookie never matters.

- **Notifications through APNs.** A WKWebView has no Web Push, so the app
  registers with Apple's push service itself (`Notifications.swift`): the
  page's ⋯ → notifications asks the app (`pushEnable`), iOS asks once for
  permission, and the page posts the device token to `tenx web`
  (`/push/apns/subscribe`). The Mac sends through APNs with the key
  `tenx web apns-key AuthKey_<id>.p8` put in place. A tap opens the task.
  An Xcode build signs up for the sandbox, a TestFlight build for
  production (`PushRegistration.env`); the entitlement is
  `TenX.entitlements`, and the App ID has Push Notifications on, so the
  App Store profile includes it.

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

Every tenx release uploads its own version of the app, for internal testers:
`.github/workflows/testflight-ios.yml` runs after the release is out
(`post-announce-jobs` in `dist-workspace.toml`). To upload again, or between
releases: `gh workflow run testflight-ios.yml`.

It runs fastlane (`fastlane/Fastfile`), and so can you:

    bundle install
    bundle exec fastlane build   # the signed build/tenx.ipa, no upload
    bundle exec fastlane beta    # build, then upload

App "tenx terminal" in App Store Connect, bundle id `it.luedeke.mobile.tenx`,
team `RUJR5DY35F`. The version is tenx's (`Cargo.toml`), the build number the
UTC time (yyyymmddHHMM); neither is written into the project.

The distribution certificate and the "match AppStore it.luedeke.mobile.tenx"
profile come from [match](https://docs.fastlane.tools/actions/match/), out of
`aluedeke/app-store-connect`. fastlane needs in the environment:

- `APP_STORE_CONNECT_API_KEY_ID` and `APP_STORE_CONNECT_ISSUER_ID`, with the
  key in `~/.appstoreconnect/private_keys/AuthKey_<id>.p8` (or, base64, in
  `APP_STORE_CONNECT_API_KEY_BASE64`);
- `MATCH_PASSWORD`, the match repository's passphrase, and read access to
  that repository over SSH.

CI takes them from `secrets/ci.enc.env`, where
`scripts/ios-testflight-secrets.sh` puts the key, the passphrase and a
read-only deploy key; an agent asks for them with `tenx secrets need`.

Xcode's export signs with the certificate named by its SHA-1, not by its
name: the team's name has a "Ü", and named, Xcode writes it into the app's
designated requirement decomposed (U + combining diaeresis) while the
certificate holds it composed, so App Store Connect rejects the upload as
"Invalid Signature".

Adding a capability to the App ID (Push Notifications, 2026-11) invalidates
its profiles: regenerate the match profile once on a Mac, with write access
to `aluedeke/app-store-connect` —
`bundle exec fastlane match appstore --app_identifier it.luedeke.mobile.tenx --force`
— or CI's read-only match fails on the old one.
