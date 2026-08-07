#!/usr/bin/env sh
# Remove what install.sh put in $XDG_DATA_HOME. Leaves the binary alone.
set -eu

APP_ID=dev.moonlightcode.MoonlightCode
data_home=${XDG_DATA_HOME:-$HOME/.local/share}
entry_dir=$data_home/applications

rm -f "$entry_dir/$APP_ID.desktop"
rm -rf "${data_home:?}/$APP_ID"

if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database "$entry_dir" >/dev/null 2>&1 || true
fi

echo "removed the MoonlightCode desktop entry and icon"
