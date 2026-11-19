#!/usr/bin/env bash
# Signs a release and publishes it. The release workflow makes each release as a draft; a draft
# stays unpublished until someone with the release key signs its SHA256SUMS here.
#
#   scripts/sign-release.sh v0.1.0
#   scripts/sign-release.sh --keygen      (once: makes the release key)
#
# Before signing, it checks, and prints for you to compare with the release workflow's run:
#   - the tag's commit, which must be on main on GitHub;
#   - every download against SHA256SUMS (their hashes), and that SHA256SUMS lists exactly them;
#   - each download's build provenance (gh attestation verify: built by this repository's release
#     workflow, from this tag), when gh can.
#
# The key never meets the build: OpenSSL signs, in the build image, in a container with no
# network, a read-only filesystem, the key mounted read-only and nothing else but a copy of what
# it signs. What's signed is a purpose line, the version and the file (scripts/verify-release.sh
# says exactly), so a signature can't be reused for another version. The signature is then
# checked by scripts/verify-release.sh, as players and operators check it, against the public key
# that script carries.
#
# The server's Docker image, which the release workflow pushed only as :<version>-unsigned, is
# signed with the release too: its digest (the image exactly, whatever the tag points at later)
# is checked against its build provenance (this repository's workflow, at the tag), written to
# IMAGE and signed for its own purpose, as IMAGE.sig; both go up with the release. Once
# published, that digest gets the image's real tags (:<version>, and :latest for a release). That
# needs gh signed in with the write:packages scope (gh auth refresh -s write:packages); otherwise
# it prints how.
#
# The key lives outside the repository, in RELEASE_KEY (default
# ~/.config/cross-com-release/release.key). --keygen makes it and writes its public half into
# scripts/verify-release.sh and docs/releasing.md (with its fingerprint), to commit.
# Needs the gh CLI signed in to the release's repository, and Docker.
set -euo pipefail
cd "$(dirname "$0")/.."
KEY=${RELEASE_KEY:-$HOME/.config/cross-com-release/release.key}
REPO=${REPO:-JDevWebb/cross-com-enhanced}
IMAGE=${IMAGE:-fes-build:local}
die() { echo "error: $*" >&2; exit 1; }
ask() { printf '%s [y/N] ' "$1"; read -r a; [ "$a" = y ] || [ "$a" = Y ]; }

# The public key as verify-release.sh carries it, or nothing.
carried_key() {
  awk '/^RELEASE_KEY_PEM=""$/ { exit } /^RELEASE_KEY_PEM="/ { on = 1 } on { print } on && /END PUBLIC KEY/ { exit }' scripts/verify-release.sh |
    sed 's/^RELEASE_KEY_PEM="//; s/"$//'
}

if [ "${1:-}" = --keygen ]; then
  [ ! -e "$KEY" ] || die "$KEY exists already; not replacing a release key"
  [ -z "$(carried_key)" ] || ask "scripts/verify-release.sh already carries a release key. Make a new one (releases signed with the old key will no longer verify)?" || exit 1
  mkdir -p "$(dirname "$KEY")"
  chmod 700 "$(dirname "$KEY")"
  (umask 077 && openssl genpkey -algorithm ed25519 -out "$KEY")
  pem=$(openssl pkey -in "$KEY" -pubout)
  fingerprint=$(openssl pkey -in "$KEY" -pubout -outform DER | openssl dgst -sha256 -r | cut -d' ' -f1)
  PEM="$pem" FINGERPRINT="$fingerprint" python3 - <<'EOF'
import os, re
pem, fp = os.environ["PEM"], os.environ["FINGERPRINT"]
p = "scripts/verify-release.sh"
s = open(p).read()
s = re.sub(r'^RELEASE_KEY_PEM=".*?"$', lambda _: f'RELEASE_KEY_PEM="{pem}"', s, count=1, flags=re.M | re.S)
open(p, "w").write(s)
p = "docs/releasing.md"
s = open(p).read()
block = f"<!-- release-key -->\n```text\n{pem}\n```\n\nSHA-256 fingerprint of the key (DER): `{fp}`\n<!-- /release-key -->"
s = re.sub(r"<!-- release-key -->.*?<!-- /release-key -->", lambda _: block, s, count=1, flags=re.S)
open(p, "w").write(s)
EOF
  echo "Made the release key: $KEY (keep it, and a backup, off GitHub)."
  echo "Its public half is now in scripts/verify-release.sh and docs/releasing.md; commit them."
  echo "Fingerprint: $fingerprint"
  exit 0
fi

TAG=${1:?usage: scripts/sign-release.sh <tag, e.g. v0.1.0> | --keygen}
VERSION=${TAG#v}
[ "$TAG" = "v$VERSION" ] && printf '%s' "$VERSION" | grep -Eqx '[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.]+)?' ||
  die "$TAG isn't a release tag like v0.1.0"
[ -f "$KEY" ] || die "no release key at $KEY (scripts/sign-release.sh --keygen makes one)"
pem=$(carried_key)
[ -n "$pem" ] || die "scripts/verify-release.sh carries no release key; nobody could check this signature"
[ "$(openssl pkey -in "$KEY" -pubout)" = "$pem" ] || die "$KEY isn't the key scripts/verify-release.sh carries"
# The key players read in the docs is the same one.
grep -qF -- "$(printf '%s\n' "$pem" | sed -n 2p)" docs/releasing.md ||
  die "docs/releasing.md doesn't give the release key verify-release.sh carries; not signing"
docker image inspect "$IMAGE" >/dev/null 2>&1 || die "no $IMAGE image here (the build image)"

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
mkdir "$work/release" "$work/sign"

# The tag's commit, on main.
commit=$(gh api "repos/$REPO/commits/$TAG" --jq .sha) || die "GitHub doesn't know $TAG"
on_main=$(gh api "repos/$REPO/compare/main...$commit" --jq .status) || die "couldn't compare $commit with main"
case "$on_main" in
  identical | behind) ;;
  *) die "$TAG ($commit) isn't on main ($on_main); not signing it" ;;
esac
echo "$TAG is commit $commit, on main."

gh release download "$TAG" --repo "$REPO" --dir "$work/release"
# What signing adds (from a run before that didn't publish): made again below.
rm -f "$work/release/SHA256SUMS.sig" "$work/release/IMAGE" "$work/release/IMAGE.sig"
# SHA256SUMS exactly as sha256sum writes it: every line "<sha256>  <file>", each download once,
# and nothing else. A loose line (a leading space, a tab) is skipped by some sha256sum -c with
# only a warning, so the check below would pass a line a looser reader could take.
sums="$work/release/SHA256SUMS"
[ -f "$sums" ] || die "the release has no SHA256SUMS"
if LC_ALL=C grep -Evx '[0-9a-f]{64}  [A-Za-z0-9._-]+' "$sums" | grep -q .; then
  die "SHA256SUMS has a line that isn't \"<sha256>  <file>\"; not signing it"
fi
if LC_ALL=C grep -q "$(printf '\r')" "$sums"; then die "SHA256SUMS has Windows line endings; not signing it"; fi
listed=$(cut -c67- "$sums" | LC_ALL=C sort)
[ -z "$(printf '%s\n' "$listed" | uniq -d)" ] || die "SHA256SUMS lists a file twice; not signing it"
present=$(cd "$work/release" && find . -mindepth 1 -maxdepth 1 ! -name SHA256SUMS -exec basename {} \; | LC_ALL=C sort)
[ "$listed" = "$present" ] || die "SHA256SUMS doesn't list exactly the release's downloads; not signing it"
# Every download against SHA256SUMS before vouching for it, with GNU sha256sum (the build image),
# which refuses what it can't read.
docker run --rm --network none -v "$work/release":/release:ro -w /release "$IMAGE" sha256sum --strict -c SHA256SUMS
echo
echo "SHA256SUMS (compare with the release workflow's \"Collect and checksum\" step):"
cat "$sums"
echo
if gh attestation verify --help 2>/dev/null | grep -q -- '--source-ref'; then
  for f in "$work/release"/*; do
    [ "$(basename "$f")" = SHA256SUMS ] && continue
    gh attestation verify "$f" --repo "$REPO" --source-ref "refs/tags/$TAG" >/dev/null ||
      die "$(basename "$f") has no build provenance from $REPO's workflow at $TAG"
  done
  echo "Every download's build provenance checks out (this repository's workflow, at $TAG)."
else
  echo "This gh can't check build provenance (gh attestation verify --source-ref); skipped."
fi

# The server's image, by digest, built by this repository's workflow from this tag.
owner=$(printf '%s' "${REPO%%/*}" | tr '[:upper:]' '[:lower:]')
image="ghcr.io/$owner/crosscom-server"
digest=$(docker buildx imagetools inspect "$image:$VERSION-unsigned" --format '{{json .Manifest}}' 2>/dev/null |
  grep -o '"digest": *"sha256:[0-9a-f]\{64\}"' | head -1 | cut -d'"' -f4 || true)
if [ -z "$digest" ]; then
  ask "$image:$VERSION-unsigned isn't there. Sign the release without its server image?" || { echo "Not signed."; exit 1; }
else
  if gh attestation verify --help 2>/dev/null | grep -q -- '--source-ref'; then
    gh attestation verify "oci://$image@$digest" --repo "$REPO" --source-ref "refs/tags/$TAG" >/dev/null ||
      die "$image@$digest has no build provenance from $REPO's workflow at $TAG; not signing it"
    echo "The server image $image@$digest: built by this repository's workflow, at $TAG."
  else
    # Without gh's check, its revision label, exactly (the workflow sets it to the commit).
    revision=$(docker buildx imagetools inspect "$image@$digest" --format '{{json .Image}}' 2>/dev/null |
      grep -o '"org.opencontainers.image.revision": *"[0-9a-f]\{40\}"' | head -1 | cut -d'"' -f4 || true)
    [ "$revision" = "$commit" ] || die "$image@$digest says it was built from ${revision:-no commit}, not $commit; not signing it"
    echo "The server image $image@$digest: labelled with $commit (this gh can't check its provenance)."
  fi
  printf '%s@%s\n' "$image" "$digest" >"$work/sign/IMAGE"
fi
ask "Sign $TAG (commit $commit)?" || { echo "Not signed."; exit 1; }

# Only OpenSSL runs with the key, as you (the key is yours alone to read): no network, no caches or
# sources, and nothing to write but the signatures.
cp "$sums" "$work/sign/SHA256SUMS"
# shellcheck disable=SC2016
docker run --rm --network none --read-only --tmpfs /tmp --cap-drop ALL --security-opt no-new-privileges \
  --user "$(id -u):$(id -g)" -e VERSION="$VERSION" -v "$KEY":/key:ro -v "$work/sign":/release "$IMAGE" sh -euc '
    sign() { # PURPOSE FILE
      { printf "%s\n%s\n" "$1" "$VERSION"; cat "/release/$2"; } > /tmp/signed
      openssl pkeyutl -sign -inkey /key -rawin -in /tmp/signed -out /tmp/signature
      openssl base64 -A -in /tmp/signature > "/release/$2.sig"
      echo >> "/release/$2.sig"
    }
    sign cross-com/release/v1 SHA256SUMS
    if [ -f /release/IMAGE ]; then sign cross-com/image/v1 IMAGE; fi
  '
# Checked as everyone else checks it: verify-release.sh, with the key it carries, against the
# downloads themselves.
cp "$work/sign/SHA256SUMS.sig" "$work/release/"
[ ! -f "$work/sign/IMAGE" ] || cp "$work/sign/IMAGE" "$work/sign/IMAGE.sig" "$work/release/"
scripts/verify-release.sh "$VERSION" "$work/release" >/dev/null ||
  die "the signature doesn't verify with scripts/verify-release.sh; not uploading it"
echo "Signed $TAG ($VERSION), and checked the signature."

if [ -f "$work/sign/IMAGE" ]; then
  gh release upload "$TAG" "$work/sign/SHA256SUMS.sig" "$work/sign/IMAGE" "$work/sign/IMAGE.sig" --repo "$REPO" --clobber
else
  gh release upload "$TAG" "$work/sign/SHA256SUMS.sig" --repo "$REPO" --clobber
fi
# PUBLISH=yes (scripts/release.sh) publishes without asking again.
if [ "${PUBLISH:-}" != yes ] && ! ask "Publish $TAG now?"; then
  echo "Signed; still a draft. Run this again to publish it (and tag its image)."
  exit 0
fi
gh release edit "$TAG" --repo "$REPO" --draft=false && echo "Published $TAG."

# The server's image: the digest signed above gets its real tags.
[ -n "$digest" ] || exit 0
case "$VERSION" in *-*) latest="" ;; *) latest="$image:latest" ;; esac
image_help="Tag it by hand once gh has the scope (gh auth refresh -s write:packages):
  gh auth token | docker login ghcr.io -u \$(gh api user --jq .login) --password-stdin
  docker buildx imagetools create --tag $image:$VERSION${latest:+ --tag $latest} $image@$digest"
if ! gh auth token | docker login ghcr.io -u "$(gh api user --jq .login)" --password-stdin >/dev/null 2>&1; then
  echo "Couldn't sign in to ghcr.io with gh's token, so the server image isn't tagged. $image_help"
  exit 0
fi
if docker buildx imagetools create --tag "$image:$VERSION" ${latest:+--tag "$latest"} "$image@$digest"; then
  echo "Tagged the server image $image@$digest as :$VERSION${latest:+ and :latest}."
else
  echo "Couldn't tag the server image (gh's token needs write:packages). $image_help"
fi
