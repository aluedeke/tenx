#!/usr/bin/env bash
# Adds the signed tenx.app (.github/workflows/build-macos-app.yml) to the
# Homebrew formula dist wrote: a resource on Apple silicon, installed into the
# keg as <prefix>/opt/tenx/tenx.app, where `tenx web service install` finds it
# (tenx_core::service::service_program). Run by publish-homebrew.yml.
#
#   scripts/formula-add-app.sh <formula.rb> <tag> <tenx-app-aarch64-apple-darwin.tar.xz>
#
# Edits the formula in place. Fails, instead of writing a formula without the
# app, when dist's formula no longer has the lines it hooks into.
set -euo pipefail

formula=$1 tag=$2 archive=$3
url="https://github.com/aluedeke/tenx/releases/download/${tag}/$(basename "$archive")"
sha=$(sha256sum "$archive" | cut -d' ' -f1)

install_line='      bin.install "tenx", "tenx-whisper", "tenx-tts"'
grep -qxF '  license any_of: ["MIT", "Apache-2.0"]' "$formula" || { echo "formula-add-app: no license line in $formula" >&2; exit 1; }
grep -qxF "$install_line" "$formula" || { echo "formula-add-app: no Apple silicon install line in $formula" >&2; exit 1; }

awk -v url="$url" -v sha="$sha" -v install_line="$install_line" '
  $0 == "  license any_of: [\"MIT\", \"Apache-2.0\"]" {
    print "  on_macos do"
    print "    on_arm do"
    print "      resource \"tenx-app\" do"
    print "        url \"" url "\""
    print "        sha256 \"" sha "\""
    print "      end"
    print "    end"
    print "  end"
  }
  { print }
  $0 == install_line {
    # Homebrew stages a one-directory archive inside that directory.
    print "      resource(\"tenx-app\").stage do"
    print "        app = (File.basename(Dir.pwd) == \"tenx.app\") ? Pathname.pwd : Pathname.pwd/\"tenx.app\""
    print "        (prefix/\"tenx.app\").install app.children"
    print "      end"
  }
' "$formula" > "$formula.tmp"
mv "$formula.tmp" "$formula"
