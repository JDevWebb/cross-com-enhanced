#!/usr/bin/env bash
# Fails if anything committed shouldn't be public: names on the private
# denylist, game files, research output or captures. Run before every push
# (scripts/install-hooks.sh installs it as the pre-push hook). Checks every
# file of HEAD, every path, and every commit message; with --history, every
# file and path of every commit on every branch too (for the first push, or
# after rewriting history).
#
# The denylist lives outside the repo, linked as .git/info/denylist (one
# case-insensitive extended regex per line, # for comments), so this script
# never names what it looks for. Without it the check fails rather than pass
# unchecked.
set -euo pipefail
cd "$(dirname "$0")/.."

denylist=.git/info/denylist
if [ ! -s "$denylist" ]; then
  echo "check-clean: $denylist is missing or empty; link the denylist there first" >&2
  exit 2
fi

patterns=$(mktemp)
trap 'rm -f "$patterns" "$patterns.paths"' EXIT
grep -v -E '^\s*(#|$)' "$denylist" >"$patterns"

history=0
[ "${1:-}" = "--history" ] && history=1

rc=0

if git grep -n -I -i -E -f "$patterns" HEAD -- . ':!scripts/check-clean.sh'; then
  echo "check-clean: the files above mention names that must stay private" >&2
  rc=1
fi

if [ $history -eq 1 ]; then
  # Every commit's files (git grep names each match by commit:path)
  if git rev-list --all | xargs git grep -n -I -i -E -f "$patterns" -- . ':!scripts/check-clean.sh'; then
    echo "check-clean: the commits' files above mention names that must stay private" >&2
    rc=1
  fi
  if git rev-list --all --objects | awk 'NF>1 { print substr($0, index($0, " ") + 1) }' | sort -u >"$patterns.paths" &&
    grep -i -E -f "$patterns" "$patterns.paths"; then
    echo "check-clean: the paths above, in some commit, mention names that must stay private" >&2
    rc=1
  fi
  if grep -i -E '\.(big[0-9]*|sat|sdf|exe|dll|pcap|pcapng|idb|i64|gzf|bndb|wor|gao)$' "$patterns.paths"; then
    echo "check-clean: the files above, in some commit, look like game files, research output or captures" >&2
    rc=1
  fi
  rm -f "$patterns.paths"
  if git rev-list --all --objects | git cat-file --batch-check='%(objecttype) %(objectsize) %(rest)' |
    awk '$1 == "blob" && $2 > 2*1024*1024 { print $3 " (" $2 " bytes)"; found=1 } END { exit !found }'; then
    echo "check-clean: the files above, in some commit, are over 2 MB" >&2
    rc=1
  fi
fi

if git log --all --format='%h %s%n%b' | grep -i -E -f "$patterns"; then
  echo "check-clean: the commit messages above mention names that must stay private" >&2
  rc=1
fi

# Game files, research tool databases and captures, by name, wherever they are
if git ls-tree -r --name-only HEAD | grep -i -E '\.(big[0-9]*|sat|sdf|exe|dll|pcap|pcapng|idb|i64|gzf|bndb|wor|gao)$'; then
  echo "check-clean: the files above look like game files, research output or captures" >&2
  rc=1
fi

# Nothing this project writes is large; a big file is probably game data
if git ls-tree -r -l HEAD | awk '$4 > 2*1024*1024 { print $5 " (" $4 " bytes)"; found=1 } END { exit !found }'; then
  echo "check-clean: the files above are over 2 MB" >&2
  rc=1
fi

[ $rc -eq 0 ] && echo "check-clean: ok"
exit $rc
