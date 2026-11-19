#!/usr/bin/env bash
# Test players against a fresh server, as CI runs them: a server with an empty database on this
# machine (HTTP on 8080, so no root is needed), three accounts, then crosscom-testbot
#   - signing in as the game does (and a wrong password refused),
#   - asking a friend, who accepts and is invited into a party,
#   - hosting a Quick Match that another finds and joins.
# Fails if any of them does, with the server's log.
#
#   scripts/bots.sh BIN_DIR          (BIN_DIR has crosscom-server and crosscom-testbot)
set -euo pipefail
bin=${1:?usage: scripts/bots.sh BIN_DIR}
exe=""
[ -f "$bin/crosscom-server.exe" ] && exe=.exe
server="$bin/crosscom-server$exe"
bot="$bin/crosscom-testbot$exe"
work=$(mktemp -d)
pid=""
cleanup() {
  [ -z "$pid" ] || kill "$pid" 2>/dev/null || true
  rm -rf "$work"
}
trap cleanup EXIT
failed() {
  echo "bots: $*" >&2
  echo "--- server log" >&2
  tail -n 80 "$work/server.log" >&2 || true
  exit 1
}

cat >"$work/crosscom-server.toml" <<EOF
public_address = "127.0.0.1"
listen_ip = "127.0.0.1"
http_port = 8080
database = "$work/crosscom.db"
EOF
for name in botalpha botbravo botcharlie; do
  "$server" --config "$work/crosscom-server.toml" add-account "$name" "pass-$name-1" >/dev/null
done
"$server" --config "$work/crosscom-server.toml" >"$work/server.log" 2>&1 &
pid=$!
for _ in $(seq 50); do
  curl -fsS -o /dev/null http://127.0.0.1:8080/api/info 2>/dev/null && break
  kill -0 "$pid" 2>/dev/null || failed "the server stopped"
  sleep 0.2
done
curl -fsS -o /dev/null http://127.0.0.1:8080/api/info || failed "the server didn't answer"

run() { # NAME ARGS...: one bot, its output kept
  local name=$1
  shift
  "$bot" 127.0.0.1 "$name" "pass-$name-1" 8080 "$@" >"$work/$name.log" 2>&1
}
check() { # NAME WHAT: a bot's run, or fail with its output
  if ! wait "$2"; then
    cat "$work/$1.log" >&2
    failed "$1 failed: $3"
  fi
  echo "bots: ok: $3"
}

run botalpha &
check botalpha $! "signs in"

run botbravo --answer 60 &
answer=$!
sleep 1
run botalpha --friend botbravo 60 &
check botalpha $! "asks a friend and invites them into a party"
check botbravo "$answer" "accepts the request and joins the party"

run botcharlie --host 60 &
host=$!
sleep 2
run botalpha --join &
check botalpha $! "finds a Quick Match and joins it"
check botcharlie "$host" "hosts a Quick Match that someone joins"
echo "bots: all passed"
