#!/bin/sh
# Archive the app and upload it to TestFlight. Signing is Xcode's automatic
# (cloud-managed) signing, authorized by the App Store Connect API key, so
# no certificate or profile has to exist on this machine first.
#
# Needs, in the environment: APP_STORE_CONNECT_API_KEY_ID,
# APP_STORE_CONNECT_ISSUER_ID, APP_STORE_CONNECT_API_KEY_BASE64 (the .p8 file,
# base64 on one line; the key's role must be Admin to create the signing
# certificate) and APPLE_TEAM_ID. With tenx: `tenx secrets need` them, then
# run this with the released .secrets.env sourced.
#
# The build number is the UTC time (yyyymmddHHMM), so every upload is new.
set -eu
cd "$(dirname "$0")"
: "${APP_STORE_CONNECT_API_KEY_ID:?}" "${APP_STORE_CONNECT_ISSUER_ID:?}" "${APP_STORE_CONNECT_API_KEY_BASE64:?}" "${APPLE_TEAM_ID:?}"

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
key="$work/AuthKey_$APP_STORE_CONNECT_API_KEY_ID.p8"
printf '%s' "$APP_STORE_CONNECT_API_KEY_BASE64" | base64 -d > "$key"
chmod 600 "$key"
auth="-allowProvisioningUpdates -authenticationKeyPath $key -authenticationKeyID $APP_STORE_CONNECT_API_KEY_ID -authenticationKeyIssuerID $APP_STORE_CONNECT_ISSUER_ID"
build=$(date -u +%Y%m%d%H%M)

# shellcheck disable=SC2086
xcodebuild archive -project TenX.xcodeproj -scheme TenX -configuration Release \
  -destination generic/platform=iOS -archivePath "$work/tenx.xcarchive" \
  DEVELOPMENT_TEAM="$APPLE_TEAM_ID" CURRENT_PROJECT_VERSION="$build" $auth
# shellcheck disable=SC2086
xcodebuild -exportArchive -archivePath "$work/tenx.xcarchive" \
  -exportOptionsPlist ExportOptions.plist -exportPath "$work/export" $auth
echo "uploaded build $build; TestFlight shows it once Apple has processed it (5–30 min)"
