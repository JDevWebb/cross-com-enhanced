# shellcheck shell=bash disable=SC2034
# Sourced by the scripts that build in the build image (check.sh, build-launcher.sh,
# dev-server.sh, launcher-shots.sh), from the repository's root.
#
# A crate's build script, or a proc macro, runs whatever code it likes in those containers. So
# they get:
#   - the source read-only, with .git hidden (a hook or a core.fsmonitor written there would run
#     on this machine at your next git command) and docker/.env hidden (the server's settings);
#   - no capabilities and no new privileges;
#   - no writable directory of this machine at all: what a build makes stays in the build volume
#     (ccb-target) until copy_out copies it out, as you, in a container with no network.
#
# The build itself still runs as root in the container: the build volumes (fe-cargo-registry,
# ccb-target, fe-xwin-cache) and the image's toolchain (which check.sh adds clippy to) are
# root's. Without capabilities, that root can write only what root owns: the volumes and the
# container's own files, nothing here.

# The source, at /src.
SOURCE_MOUNTS=(--mount "type=bind,src=$PWD,dst=/src,readonly")
if [ -d .git ]; then
  SOURCE_MOUNTS+=(--mount "type=tmpfs,dst=/src/.git")
elif [ -e .git ]; then
  # A worktree's .git is a file naming the repository's git directory (not mounted).
  SOURCE_MOUNTS+=(--mount "type=bind,src=/dev/null,dst=/src/.git,readonly")
fi
[ ! -e docker/.env ] || SOURCE_MOUNTS+=(--mount "type=bind,src=/dev/null,dst=/src/docker/.env,readonly")
# For every container that builds.
CONFINED=(--cap-drop ALL --security-opt no-new-privileges)

# copy_out IMAGE FILE DIR: copies FILE (a path in the build volume, under /target) into DIR here.
copy_out() {
  mkdir -p "$3"
  docker run --rm --network none --cap-drop ALL --security-opt no-new-privileges --user "$(id -u):$(id -g)" \
    -v ccb-target:/target:ro --mount "type=bind,src=$(cd "$3" && pwd),dst=/out" "$1" cp "$2" /out/
}
