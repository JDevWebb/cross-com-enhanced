#!/usr/bin/env bash
# Checks the workspace as CI would: formatting, clippy (as errors) and the tests, in the build
# image (IMAGE, default fes-build:local), then the same for the launcher (launcher-slint, a
# workspace of its own). TARGET is where the build volume's outputs go inside the image (default
# /target). Run before committing.
#
#   scripts/check.sh
set -euo pipefail
cd "$(dirname "$0")/.."
image=${IMAGE:-fes-build:local}
target=${TARGET:-/target}
crates=(-p quazal -p crosscom-server -p crosscom-testbot -p crosscom-launcher -p crosscom-client-common)
docker run --rm -v "$PWD":/src -w /src -v fe-cargo-registry:/usr/local/cargo/registry -v ccb-target:/target \
  -e CARGO_TARGET_DIR="$target" "$image" sh -euc "
    rustup component add clippy rustfmt >/dev/null 2>&1
    cargo fmt --check ${crates[*]}
    cargo clippy -q ${crates[*]} --all-targets -- -D warnings
    cargo test -q ${crates[*]}
    cd launcher-slint
    export CARGO_TARGET_DIR=$target/launcher-slint-native
    cargo fmt --check
    cargo clippy -q --all-targets -- -D warnings
    cargo test -q
  "
echo "check: ok"
