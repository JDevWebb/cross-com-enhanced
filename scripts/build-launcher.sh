#!/usr/bin/env bash
# Builds the launcher for Windows -> dist/crosscom-launcher.exe, the one file players need: the
# client DLLs first (32-bit, as the game), then the launcher's page (launcher-tauri/ui), then the
# launcher with both built in. Everything in the build image (IMAGE, default fes-build:local).
#
#   scripts/build-launcher.sh
set -euo pipefail
cd "$(dirname "$0")/.."
image=${IMAGE:-fes-build:local}
mkdir -p dist
run() {
  docker run --rm -v "$PWD":/src -w /src -v fe-cargo-registry:/usr/local/cargo/registry \
    -v fe-xwin-cache:/root/.cache/cargo-xwin -v ccb-target:/target -v fes-npm-cache:/root/.npm \
    -e XWIN_ACCEPT_LICENSE=1 -e XWIN_ARCH=x86,x86_64 "$image" sh -euc "$1"
}
run '
  # The client DLLs the launcher carries.
  CARGO_TARGET_DIR=/target/win cargo xwin build -q --release -p crosscom-orbit -p crosscom-uplay --target i686-pc-windows-msvc
  mkdir -p /target/client
  cp /target/win/i686-pc-windows-msvc/release/ubiorbitapi_r2_loader.dll /target/win/i686-pc-windows-msvc/release/uplay_r1_loader.dll /target/client/

  # Its page.
  cd launcher-tauri/ui
  if [ ! -d node_modules ] || [ package-lock.json -nt node_modules ]; then npm ci --ignore-scripts --no-audit --no-fund --prefer-offline; touch node_modules; fi
  npm run build >/dev/null

  # The launcher, with both built in.
  cd ../src-tauri
  rustup target add x86_64-pc-windows-msvc >/dev/null 2>&1
  CARGO_TARGET_DIR=/target/launcher-tauri CROSSCOM_CLIENT_DIR=/target/client \
    cargo xwin build -q --release --features embed-dlls --target x86_64-pc-windows-msvc
  cp /target/launcher-tauri/x86_64-pc-windows-msvc/release/crosscom-launcher.exe /src/dist/
'
ls -la dist/crosscom-launcher.exe
