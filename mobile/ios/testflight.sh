#!/bin/sh
# Archive the app, sign it for the App Store and upload it to TestFlight.
#
# Signed by hand, not by Xcode's export: the team's certificate name has a
# "Ü", and Xcode writes it into the app's designated requirement decomposed
# (U + combining diaeresis) while the certificate holds it composed, so App
# Store Connect rejects the upload as "Invalid Signature". The requirement
# below names the team (subject.OU) instead of the name, and the app is
# packed and uploaded as signed here.
#
# Needs, once per machine (see README.md, "TestFlight"):
#   - the "Apple Distribution" identity in $TENX_SIGNING_KEYCHAIN, with its
#     password in $TENX_SIGNING_KEYCHAIN_PASS_FILE;
#   - the "tenx App Store" provisioning profile installed;
#   - nothing else: APP_STORE_CONNECT_API_KEY_ID, APP_STORE_CONNECT_ISSUER_ID
#     and APP_STORE_CONNECT_API_KEY_BASE64 (the .p8) are sealed in
#     secrets/ios.enc.env. Taken from the environment if set, else from
#     secrets/ios.env (`tenx secrets need APP_STORE_CONNECT_ISSUER_ID …`
#     releases it there), else decrypted by sops with your age identity.
#
# The build number is the UTC time (yyyymmddHHMM), so every upload is new.
set -eu
cd "$(dirname "$0")"
sealed=../../secrets/ios.enc.env
if [ -z "${APP_STORE_CONNECT_ISSUER_ID:-}" ]; then
  if [ -f ../../secrets/ios.env ]; then
    set -a
    . ../../secrets/ios.env
    set +a
  else
    # The values only ever live in this process's environment.
    exec sops exec-env "$sealed" "sh '$PWD/testflight.sh'"
  fi
fi
: "${APP_STORE_CONNECT_API_KEY_ID:?}" "${APP_STORE_CONNECT_ISSUER_ID:?}"
team=${APPLE_TEAM_ID:-RUJR5DY35F}
dir=$HOME/.appstoreconnect/tenx-signing
keychain=${TENX_SIGNING_KEYCHAIN:-$dir/tenx-signing.keychain-db}
pass_file=${TENX_SIGNING_KEYCHAIN_PASS_FILE:-$dir/kc.pass}
security unlock-keychain -p "$(cat "$pass_file")" "$keychain"

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
build=$(date -u +%Y%m%d%H%M)
# altool finds the key as AuthKey_<id>.p8 in API_PRIVATE_KEYS_DIR.
if [ -n "${APP_STORE_CONNECT_API_KEY_BASE64:-}" ]; then
  mkdir -m 700 "$work/private_keys"
  printf %s "$APP_STORE_CONNECT_API_KEY_BASE64" | base64 -d >"$work/private_keys/AuthKey_$APP_STORE_CONNECT_API_KEY_ID.p8"
  export API_PRIVATE_KEYS_DIR="$work/private_keys"
fi
req="=designated => anchor apple generic and identifier \"it.luedeke.mobile.tenx\" and certificate leaf[subject.OU] = \"$team\" and certificate 1[field.1.2.840.113635.100.6.2.1] exists"

xcodebuild archive -project TenX.xcodeproj -scheme TenX -configuration Release \
  -destination generic/platform=iOS -archivePath "$work/tenx.xcarchive" \
  DEVELOPMENT_TEAM="$team" CURRENT_PROJECT_VERSION="$build" CODE_SIGN_STYLE=Manual \
  CODE_SIGN_IDENTITY="Apple Distribution" PROVISIONING_PROFILE_SPECIFIER="tenx App Store" \
  OTHER_CODE_SIGN_FLAGS="--keychain $keychain --requirements '$req'"

app="$work/tenx.xcarchive/Products/Applications/tenx.app"
# The requirement above names the app's identifier, and Xcode put it on the
# embedded frameworks too (WebRTC), whose identifier differs: Apple rejects
# such a framework as "not properly signed". Each framework gets one naming
# its own identifier, then the app is signed again over them.
sign() {
  codesign --force --sign "Apple Distribution" --keychain "$keychain" --timestamp "$@"
}
for fw in "$app"/Frameworks/*.framework; do
  [ -d "$fw" ] || continue
  id=$(/usr/libexec/PlistBuddy -c 'Print :CFBundleIdentifier' "$fw/Info.plist")
  sign --requirements "=designated => anchor apple generic and identifier \"$id\" and certificate leaf[subject.OU] = \"$team\" and certificate 1[field.1.2.840.113635.100.6.2.1] exists" "$fw"
done
sign --preserve-metadata=entitlements --requirements "$req" "$app"
codesign --verify --deep --strict "$app"
mkdir "$work/Payload"
cp -R "$app" "$work/Payload/"
(cd "$work" && zip -qry tenx.ipa Payload)
xcrun altool --upload-app -f "$work/tenx.ipa" -t ios \
  --apiKey "$APP_STORE_CONNECT_API_KEY_ID" --apiIssuer "$APP_STORE_CONNECT_ISSUER_ID"
echo "uploaded build $build; TestFlight shows it once Apple has processed it (5–30 min)"
