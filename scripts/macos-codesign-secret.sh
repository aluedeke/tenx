#!/usr/bin/env bash
# Puts the Developer ID Application certificate and its key into
# secrets/ci.enc.env for .github/workflows/build-macos-app.yml:
#   MACOS_CODESIGN_P12           the identity as a .p12, base64
#   MACOS_CODESIGN_P12_PASSWORD  that .p12's password
# Run it yourself, in a terminal, from this repo; then commit
# secrets/ci.enc.env. Again when the certificate is renewed.
#
#   scripts/macos-codesign-secret.sh
#
# `security export` takes every identity out of the login keychain; macOS
# asks in a window of its own to allow it (a key export needs that, and only
# Apple's tools may show it) and for a password for the export, which the
# script then asks for once more. It keeps the one Developer ID Application
# identity, packs it into a .p12 of its own with a random password, and writes
# both into the secrets with sops. First it asks for the passphrase of your
# age identity, ~/.config/age/keys.txt, unless SOPS_AGE_KEY* is set.
# Nothing secret is printed, passed on a command line or left on disk.
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"
test -f secrets/ci.enc.env
keychain=~/Library/Keychains/login.keychain-db

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
sops decrypt secrets/ci.enc.env > /dev/null || { echo "sops can't open secrets/ci.enc.env with that identity" >&2; exit 1; }

echo "exporting from the login keychain: allow it in the macOS window, and choose a password for the export there..."
security export -k "$keychain" -t identities -f pkcs12 -o "$tmp/all.p12"
read -r -s -p "the password you just chose for the export: " EXPORT_PASSWORD
echo
export EXPORT_PASSWORD

# OpenSSL 3+ needs -legacy for the older encryption the keychain writes.
unpack() { openssl pkcs12 -in "$tmp/all.p12" -passin env:EXPORT_PASSWORD -nodes 2>/dev/null || openssl pkcs12 -legacy -in "$tmp/all.p12" -passin env:EXPORT_PASSWORD -nodes 2>/dev/null; }
unpack > "$tmp/all.pem" || { echo "can't open the export with that password" >&2; exit 1; }

# Split into <localKeyID>.cert / <localKeyID>.key: a certificate and its key
# share the localKeyID bag attribute; a bag without one is skipped. LC_ALL=C:
# a name like "LÜDEKE" comes out of openssl in bytes that aren't valid in the
# UTF-8 locale.
LC_ALL=C awk -v dir="$tmp" '
  /^Bag Attributes/ { id = "" }
  /localKeyID:/ { sub(/.*localKeyID: */, ""); gsub(/ /, ""); id = $0 }
  /^-----BEGIN CERTIFICATE-----/ && id != "" { out = dir "/" id ".cert" }
  /^-----BEGIN .*PRIVATE KEY-----/ && id != "" { out = dir "/" id ".key" }
  out != "" { print > out }
  /^-----END / { close(out); out = "" }
' "$tmp/all.pem"
rm -f "$tmp/all.pem" "$tmp/all.p12"

found=()
for cert in "$tmp"/*.cert; do
  [[ -e "$cert" ]] || continue
  subject=$(openssl x509 -in "$cert" -noout -subject -nameopt utf8 2>/dev/null || true)
  if [[ "$subject" == *"Developer ID Application"* ]] && openssl x509 -in "$cert" -noout -checkend 0 >/dev/null 2>&1 && [[ -f "${cert%.cert}.key" ]]; then
    found+=("$cert")
    echo "found: ${subject#subject=}"
  fi
done
case ${#found[@]} in
  1) ;;
  0) echo "no valid Developer ID Application certificate with its key in the login keychain" >&2; exit 1 ;;
  *) echo "more than one valid Developer ID Application certificate: delete the ones CI shouldn't use, then run this again" >&2; exit 1 ;;
esac
cert=${found[0]}

# A .p12 of its own, with algorithms `security import` on the runner reads.
P12_PASSWORD=$(openssl rand -hex 24)
export P12_PASSWORD
openssl pkcs12 -export -in "$cert" -inkey "${cert%.cert}.key" -name "Developer ID Application" \
  -certpbe PBE-SHA1-3DES -keypbe PBE-SHA1-3DES -macalg sha1 \
  -passout env:P12_PASSWORD -out "$tmp/codesign.p12"
rm -f "$tmp"/*.key

# sops set reads a JSON value from the file.
printf '"%s"' "$(base64 < "$tmp/codesign.p12" | tr -d '\n')" > "$tmp/p12.json"
printf '"%s"' "$P12_PASSWORD" > "$tmp/password.json"
echo "writing them into secrets/ci.enc.env..."
sops set --value-file secrets/ci.enc.env '["MACOS_CODESIGN_P12"]' "$tmp/p12.json"
sops set --value-file secrets/ci.enc.env '["MACOS_CODESIGN_P12_PASSWORD"]' "$tmp/password.json"

echo "done: secrets/ci.enc.env now has $(grep -c '^MACOS_CODESIGN_' secrets/ci.enc.env) MACOS_CODESIGN_* entries. Commit secrets/ci.enc.env."
