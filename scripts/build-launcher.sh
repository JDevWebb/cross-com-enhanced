#!/usr/bin/env bash
# Builds the launcher for Windows -> dist/crosscom-launcher.exe, the one file players need: the
# client DLLs first (32-bit, as the game) and the server (Host runs it), then the launcher
# (launcher-slint) with them built in. The server goes to dist/ on its own too.
# Everything in the build image (IMAGE, default fes-build:local); TARGET is where the build
# volume's outputs go inside it (default /target). The build gets the source read-only, without
# .git, and the exe is copied out afterwards (scripts/build-container.sh).
#
#   scripts/build-launcher.sh
set -euo pipefail
cd "$(dirname "$0")/.."
. scripts/build-container.sh
image=${IMAGE:-fes-build:local}
target=${TARGET:-/target}
docker run --rm "${CONFINED[@]}" "${SOURCE_MOUNTS[@]}" -w /src -v fe-cargo-registry:/usr/local/cargo/registry \
  -v fe-xwin-cache:/root/.cache/cargo-xwin -v ccb-target:/target \
  -e XWIN_ACCEPT_LICENSE=1 -e XWIN_ARCH=x86,x86_64 -e T="$target" "$image" sh -euc '
  # The client DLLs the launcher carries.
  CARGO_TARGET_DIR=$T/win cargo xwin build -q --release -p crosscom-orbit -p crosscom-uplay --target i686-pc-windows-msvc
  mkdir -p $T/client
  cp $T/win/i686-pc-windows-msvc/release/ubiorbitapi_r2_loader.dll $T/win/i686-pc-windows-msvc/release/uplay_r1_loader.dll $T/client/
  # The server, for Host.
  CARGO_TARGET_DIR=$T/win cargo xwin build -q --release -p crosscom-server --target x86_64-pc-windows-msvc
  mkdir -p $T/server
  cp $T/win/x86_64-pc-windows-msvc/release/crosscom-server.exe $T/server/

  # The launcher, with them built in.
  cd launcher-slint
  rustup target add x86_64-pc-windows-msvc >/dev/null 2>&1
  CARGO_TARGET_DIR=$T/launcher-slint CROSSCOM_CLIENT_DIR=$T/client CROSSCOM_SERVER_DIR=$T/server \
    cargo xwin build -q --release --features embed-dlls,embed-server --target x86_64-pc-windows-msvc
'
copy_out "$image" "$target/launcher-slint/x86_64-pc-windows-msvc/release/crosscom-launcher.exe" dist
copy_out "$image" "$target/server/crosscom-server.exe" dist
ls -la dist/crosscom-launcher.exe dist/crosscom-server.exe
