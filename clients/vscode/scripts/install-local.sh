#!/usr/bin/env bash
# Builds, packages and installs every extension into a local editor: install-local.sh
#
# For trying a version before it is tagged. Installs in the order publish-order.js
# gives — core first, the pack last — because an extension whose
# `extensionDependencies` are not installed yet fails to activate until a reload.
#
# `--force` because the marketplace build of the same extensions is usually already
# installed; without it, installing a same-version .vsix over it is a no-op.
#
# A locally packaged .vsix carries no daemon pins (see README), so it never downloads
# `moonlightd`: it uses the one on PATH, `moonlight.daemon.path`, or the desktop app's.
#
# EDITOR_CLI overrides the editor, e.g. `EDITOR_CLI=cursor npm run install:local`.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root"

cli="${EDITOR_CLI:-}"
if [ -z "$cli" ]; then
  if command -v code >/dev/null 2>&1; then
    cli="code"
  elif [ -x "/Applications/Visual Studio Code.app/Contents/Resources/app/bin/code" ]; then
    cli="/Applications/Visual Studio Code.app/Contents/Resources/app/bin/code"
  else
    echo "No editor CLI found: put \`code\` on PATH or set EDITOR_CLI." >&2
    exit 1
  fi
fi

npm run compile
# Stale .vsix from an earlier version would be installed alongside the new ones.
find extensions -maxdepth 2 -name '*.vsix' -delete
npm run package

out="$(mktemp -d)"
trap 'rm -rf "$out"' EXIT
find extensions -maxdepth 2 -name '*.vsix' -exec cp {} "$out/" \;

node scripts/publish-order.js "$out" | while read -r pkg; do
  echo "--> $(basename "$pkg")"
  "$cli" --install-extension "$pkg" --force
done

echo "Installed. Run 'Developer: Reload Window' in every open window to load them."
