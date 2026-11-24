#!/usr/bin/env bash
# Checks a Cross-Com Enhanced release you downloaded: that SHA256SUMS is signed by the release key
# for that version (SHA256SUMS.sig), then every file of the release you have against SHA256SUMS.
# With IMAGE and IMAGE.sig there too, also the server image's signature, and prints the image to
# pull, by digest. sign-release.sh runs it on each signature it makes.
#
#   scripts/verify-release.sh 0.1.0 [DIR]       (DIR: the folder with the downloads; default .)
#
# Needs bash, OpenSSL 3 (for Ed25519) and sha256sum or shasum: Linux, macOS, or on Windows Git
# Bash (Git for Windows ships bash, coreutils and OpenSSL 3). It prints the release key's
# fingerprint: compare it with the one the maintainer publishes outside this repository
# (docs/releasing.md says where), so a changed copy of this script can't pass with another key.
set -euo pipefail

# The release key's public half. sign-release.sh --keygen writes it here and in docs/releasing.md
# (with its fingerprint); a release is signed with its private half, which stays off GitHub.
RELEASE_KEY_PEM="-----BEGIN PUBLIC KEY-----
MCowBQYDK2VwAyEAz8ux0mceIUIneqPhVCDoQaKcSWPIiUwa//3bdhjkYW4=
-----END PUBLIC KEY-----"

VERSION=${1:?usage: scripts/verify-release.sh <version, e.g. 0.1.0> [dir]}
DIR=${2:-.}
VERSION=${VERSION#v}
die() { echo "verify-release: $*" >&2; exit 1; }
[ -n "$RELEASE_KEY_PEM" ] || die "this copy has no release key yet (sign-release.sh --keygen)"
printf '%s' "$VERSION" | grep -Eqx '[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.]+)?' || die "$VERSION isn't a version like 0.1.0"
command -v openssl >/dev/null || die "needs openssl"
openssl version | grep -Eq '^OpenSSL [3-9]' || die "needs OpenSSL 3 or later for Ed25519 ($(openssl version))"
[ -f "$DIR/SHA256SUMS" ] && [ -f "$DIR/SHA256SUMS.sig" ] || die "no SHA256SUMS and SHA256SUMS.sig in $DIR"

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
printf '%s\n' "$RELEASE_KEY_PEM" >"$work/release.pem"
fingerprint=$(openssl pkey -pubin -in "$work/release.pem" -outform DER | openssl dgst -sha256 -r | cut -d' ' -f1)
echo "Release key fingerprint (SHA-256, DER): $fingerprint"

# What's signed: a purpose line, the version (so a release can't be passed off as another), then
# the file exactly. The signature is base64, on one line.
verify() { # PURPOSE FILE SIG
  { printf '%s\n%s\n' "$1" "$VERSION"; cat "$2"; } >"$work/signed"
  tr -d '[:space:]' <"$3" | openssl base64 -d -A >"$work/signature" 2>/dev/null || return 1
  openssl pkeyutl -verify -pubin -inkey "$work/release.pem" -rawin -in "$work/signed" -sigfile "$work/signature" >/dev/null 2>&1
}

verify cross-com/release/v1 "$DIR/SHA256SUMS" "$DIR/SHA256SUMS.sig" ||
  die "SHA256SUMS.sig isn't the release key's signature of $VERSION's SHA256SUMS: don't use these files"
echo "SHA256SUMS: signed by the release key for $VERSION."

if command -v sha256sum >/dev/null; then sum=(sha256sum); else sum=(shasum -a 256); fi
checked=0
while read -r hash name; do
  [ -f "$DIR/$name" ] || continue
  [ "$("${sum[@]}" "$DIR/$name" | cut -d' ' -f1)" = "$hash" ] || die "$name doesn't match SHA256SUMS: don't use it"
  echo "$name: matches."
  checked=$((checked + 1))
done <"$DIR/SHA256SUMS"
[ "$checked" -gt 0 ] || die "none of the files SHA256SUMS lists are in $DIR"

if [ -f "$DIR/IMAGE" ] || [ -f "$DIR/IMAGE.sig" ]; then
  [ -f "$DIR/IMAGE" ] && [ -f "$DIR/IMAGE.sig" ] || die "IMAGE and IMAGE.sig go together"
  verify cross-com/image/v1 "$DIR/IMAGE" "$DIR/IMAGE.sig" ||
    die "IMAGE.sig isn't the release key's signature of $VERSION's server image: don't use it"
  echo "The server image, signed for $VERSION: $(cat "$DIR/IMAGE")"
fi
echo "verify-release: $checked file(s) of $VERSION check out."
