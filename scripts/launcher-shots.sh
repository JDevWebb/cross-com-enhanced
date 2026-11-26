#!/usr/bin/env bash
# The README's launcher screenshots -> docs/screenshots/launcher-{play,setup,settings,host}.png: the
# launcher's screens filled in from made-up state and drawn by Slint's software renderer, with
# no screen and no network (launcher-slint/src/shots.rs). In the build image (IMAGE, default
# fes-build:local); TARGET as for build-launcher.sh. The build gets the source read-only, without
# .git (scripts/build-container.sh); the screenshots are drawn as you, into docs/screenshots only.
#
#   scripts/launcher-shots.sh
set -euo pipefail
cd "$(dirname "$0")/.."
. scripts/build-container.sh
image=${IMAGE:-fes-build:local}
target=${TARGET:-/target}
docker run --rm "${CONFINED[@]}" "${SOURCE_MOUNTS[@]}" -w /src/launcher-slint -v fe-cargo-registry:/usr/local/cargo/registry \
  -v ccb-target:/target -e CARGO_TARGET_DIR="$target/launcher-slint-native" "$image" cargo build -q
mkdir -p docs/screenshots
docker run --rm --network none "${CONFINED[@]}" --user "$(id -u):$(id -g)" -e HOME=/tmp -v ccb-target:/target:ro \
  --mount "type=bind,src=$PWD/docs/screenshots,dst=/shots" "$image" \
  "$target/launcher-slint-native/debug/crosscom-launcher" --shots /shots
