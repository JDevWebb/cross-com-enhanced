#!/bin/sh
# THIRD-PARTY-NOTICES.txt for a release: the release workflow runs it on its runner. Needs
# cargo-about (pinned in release.yml: cargo install --locked cargo-about --version 0.9.3
# --features cli) and Node.
#
#   scripts/notices.sh OUT [WORK_DIR]
#
# Each released program's crates, for the targets it's released for (about.toml), as cargo-about
# JSON in WORK_DIR, merged by build/notices.js with the fonts' licences.
set -eu
cd "$(dirname "$0")/.."
out=${1:?the file to write}
work=${2:-$(mktemp -d)}
mkdir -p "$work"
about() { # NAME MANIFEST ARGS... (--features before the targets when needed)
  name=$1 manifest=$2
  shift 2
  echo "notices: $name" >&2
  cargo about generate --locked -c about.toml -m "$manifest" --format json -o "$work/$name.json" "$@" 2>"$work/$name.log" ||
    { cat "$work/$name.log" >&2; exit 1; }
}
about orbit client/orbit/Cargo.toml --target i686-pc-windows-msvc
about uplay client/uplay/Cargo.toml --target i686-pc-windows-msvc
about server server/crosscom-server/Cargo.toml --target x86_64-unknown-linux-gnu
about launcher launcher-slint/Cargo.toml --features embed-dlls --target x86_64-pc-windows-msvc
node build/notices.js "$out" assets/fonts "$work"/*.json
