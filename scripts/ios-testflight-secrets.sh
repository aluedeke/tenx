#!/usr/bin/env bash
# Puts what .github/workflows/testflight-ios.yml needs to sign the iOS app and
# upload it to TestFlight into secrets/ci.enc.env:
#   APP_STORE_CONNECT_API_KEY_BASE64  the App Store Connect API key (.p8), base64
#   MATCH_PASSWORD                    the passphrase of the match repository
#   MATCH_DEPLOY_KEY_BASE64           a new read-only deploy key on it, base64
# Run it yourself, in a terminal, from this repo; then commit
# secrets/ci.enc.env. Again to replace the deploy key or after the API key or
# the match passphrase changed (delete the old "tenx CI" deploy key on the
# match repository then).
#
#   scripts/ios-testflight-secrets.sh
#
# The API key is the one APP_STORE_CONNECT_API_KEY_ID in the secrets names,
# read from ~/.appstoreconnect/private_keys/AuthKey_<id>.p8. The match
# repository is aluedeke/app-store-connect (mobile/ios/fastlane/Matchfile);
# `gh` adds the deploy key to it, so it must be logged in as someone who may.
# First it asks for the passphrase of your age identity,
# ~/.config/age/keys.txt, unless SOPS_AGE_KEY* is set.
# Nothing secret is printed, passed on a command line or left on disk.
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"
test -f secrets/ci.enc.env
match_repo=aluedeke/app-store-connect

tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT
chmod 700 "$tmp"
umask 077

# The maintainer's age identity (.sops.yaml): sops itself looks only in its own
# directory and can't read a passphrase-protected file, so unwrap it once, into
# the environment, and check it opens the secrets before anything else. A
# plain identity file sops reads as it is.
if [[ -z "${SOPS_AGE_KEY:-}${SOPS_AGE_KEY_FILE:-}${SOPS_AGE_KEY_CMD:-}" && -f ~/.config/age/keys.txt ]]; then
  if grep -q '^AGE-SECRET-KEY-' ~/.config/age/keys.txt; then
    export SOPS_AGE_KEY_FILE=~/.config/age/keys.txt
  else
    echo "unlocking your age identity (~/.config/age/keys.txt):"
    SOPS_AGE_KEY=$(age -d ~/.config/age/keys.txt)
    export SOPS_AGE_KEY
  fi
fi
key_id=$(sops decrypt --extract '["APP_STORE_CONNECT_API_KEY_ID"]' secrets/ci.enc.env) || { echo "sops can't read APP_STORE_CONNECT_API_KEY_ID from secrets/ci.enc.env with that identity" >&2; exit 1; }

p8=~/.appstoreconnect/private_keys/AuthKey_${key_id}.p8
test -f "$p8" || { echo "no API key file $p8" >&2; exit 1; }
grep -q 'BEGIN PRIVATE KEY' "$p8" || { echo "$p8 is not a .p8 key" >&2; exit 1; }

read -r -s -p "the match passphrase (MATCH_PASSWORD, as for the other apps in $match_repo): " MATCH_PASSWORD
echo
test -n "$MATCH_PASSWORD" || { echo "no passphrase given" >&2; exit 1; }

echo "adding a read-only deploy key \"tenx CI\" to $match_repo..."
ssh-keygen -q -t ed25519 -N "" -C "tenx CI (TestFlight)" -f "$tmp/match_key"
gh repo deploy-key add "$tmp/match_key.pub" --repo "$match_repo" --title "tenx CI"

# sops set reads a JSON value from the file; jq writes the passphrase as one,
# whatever characters it has.
printf '"%s"' "$(base64 < "$p8" | tr -d '\n')" > "$tmp/p8.json"
printf '"%s"' "$(base64 < "$tmp/match_key" | tr -d '\n')" > "$tmp/deploy.json"
printf '%s' "$MATCH_PASSWORD" | jq -Rs . > "$tmp/password.json"
echo "writing them into secrets/ci.enc.env..."
sops set --value-file secrets/ci.enc.env '["APP_STORE_CONNECT_API_KEY_BASE64"]' "$tmp/p8.json"
sops set --value-file secrets/ci.enc.env '["MATCH_PASSWORD"]' "$tmp/password.json"
sops set --value-file secrets/ci.enc.env '["MATCH_DEPLOY_KEY_BASE64"]' "$tmp/deploy.json"

echo "done: secrets/ci.enc.env now has APP_STORE_CONNECT_API_KEY_BASE64, MATCH_PASSWORD and MATCH_DEPLOY_KEY_BASE64. Commit secrets/ci.enc.env."
