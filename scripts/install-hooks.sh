#!/usr/bin/env bash
# Installs the pre-push hook that runs scripts/check-clean.sh.
set -euo pipefail
cd "$(dirname "$0")/.."
hook=.git/hooks/pre-push
cat >"$hook" <<'HOOK'
#!/usr/bin/env bash
exec "$(git rev-parse --show-toplevel)/scripts/check-clean.sh"
HOOK
chmod +x "$hook"
echo "installed $hook"
