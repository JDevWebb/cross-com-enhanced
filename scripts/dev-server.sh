#!/usr/bin/env bash
# Runs crosscom-server for testing, in the build container, with its ports published on this
# machine: TCP 80 (online config), UDP 21030 (auth) and 21031 (secure).
#
#   scripts/dev-server.sh PUBLIC_ADDRESS [NAME PASSWORD]
#
# PUBLIC_ADDRESS is this machine's address as the game's PC reaches it. NAME and PASSWORD create
# that account first. The database is kept in the Docker volume ccb-devdata. The build gets the
# source read-only, without .git (scripts/build-container.sh); the server doesn't get it at all.
set -euo pipefail
cd "$(dirname "$0")/.."
[ $# -ge 1 ] || { sed -n '2,9p' "$0"; exit 2; }
. scripts/build-container.sh
public=$1
image=${IMAGE:-fes-build:local}
run() {
  docker run --rm "$@"
}
config="public_address = \"$public\"
database = \"/data/crosscom.db\"
log_level = \"${LOG_LEVEL:-info}\""
run "${CONFINED[@]}" "${SOURCE_MOUNTS[@]}" -w /src -v fe-cargo-registry:/usr/local/cargo/registry -v ccb-target:/target \
  -e CARGO_TARGET_DIR=/target "$image" cargo build -q -p crosscom-server
mounts=(-v ccb-target:/target:ro -v ccb-devdata:/data)
run --network none "${mounts[@]}" -e CONFIG="$config" "$image" sh -c 'printf "%s\n" "$CONFIG" > /data/crosscom-server.toml'
if [ $# -ge 3 ]; then
  run --network none "${mounts[@]}" "$image" /target/debug/crosscom-server --config /data/crosscom-server.toml add-account "$2" "$3" || true
fi
tty=(); [ -t 0 ] && tty=(-it)
exec docker run --rm ${tty[@]+"${tty[@]}"} --name crosscom-dev "${mounts[@]}" -p 80:80 -p 21030:21030/udp -p 21031:21031/udp "$image" \
  /target/debug/crosscom-server --config /data/crosscom-server.toml
