#!/usr/bin/env bash
# The README's launcher screenshots -> docs/screenshots/launcher-{play,setup,settings}.png: the
# launcher's screens filled in from made-up state and drawn by Slint's software renderer, with
# no screen and no network (launcher-slint/src/shots.rs). In the build image (IMAGE, default
# fes-build:local); TARGET as for build-launcher.sh.
#
#   scripts/launcher-shots.sh
set -euo pipefail
cd "$(dirname "$0")/.."
image=${IMAGE:-fes-build:local}
target=${TARGET:-/target}
docker run --rm -v "$PWD":/src -w /src/launcher-slint -v fe-cargo-registry:/usr/local/cargo/registry -v ccb-target:/target \
  -e CARGO_TARGET_DIR="$target/launcher-slint-native" "$image" cargo build -q
docker run --rm --network none -v "$PWD":/src -v ccb-target:/target "$image" \
  "$target/launcher-slint-native/debug/crosscom-launcher" --shots /src/docs/screenshots
