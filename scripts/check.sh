#!/usr/bin/env bash
# Checks the workspace as CI would: formatting, clippy (as errors) and the tests, in the build
# image (IMAGE, default fes-build:local). Run before committing.
#
#   scripts/check.sh
set -euo pipefail
cd "$(dirname "$0")/.."
image=${IMAGE:-fes-build:local}
crates=(-p quazal -p crosscom-server -p crosscom-testbot -p crosscom-launcher -p crosscom-client-common)
docker run --rm -v "$PWD":/src -w /src -v fe-cargo-registry:/usr/local/cargo/registry -v ccb-target:/target \
  -e CARGO_TARGET_DIR=/target "$image" sh -euc "
    rustup component add clippy rustfmt >/dev/null 2>&1
    cargo fmt --check ${crates[*]}
    cargo clippy -q ${crates[*]} --all-targets -- -D warnings
    cargo test -q ${crates[*]}
  "
echo "check: ok"
