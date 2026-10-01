#!/usr/bin/env bash
# Publishes one .vsix with vsce or ovsx: publish-vsix.sh <vsce|ovsx> <path.vsix>
#
# A version the registry already has is a warning, not a failure: re-running a
# release whose publish half-succeeded would otherwise stop at the first
# already-live extension and never ship the ones still missing. Not
# `--skip-duplicate`: that is silent, and a skipped publish should stay visible
# in the run summary. Every other failure (auth, validation, network) still fails.
set -euo pipefail

tool="$1"
pkg="$2"
log="$(mktemp)"
trap 'rm -f "$log"' EXIT

if "./node_modules/.bin/$tool" publish --packagePath "$pkg" 2>&1 | tee "$log"; then
  exit 0
fi
if grep -qiE "already (exists|published)" "$log"; then
  echo "::warning::$tool: $(basename "$pkg") is already published at this version — skipped."
  exit 0
fi
exit 1
