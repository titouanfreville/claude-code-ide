#!/usr/bin/env sh
# Install the MoonlightCode desktop entry + icon for the current user, so the app
# grid, dash and taskbar show the product name and icon instead of the `moonlight`
# binary name and a generic placeholder.
#
#   ./packaging/linux/install.sh [path-to-moonlight-binary]
#
# With no argument the binary is looked up on PATH, then in this repo's release and
# debug target directories. Everything lands under $XDG_DATA_HOME (~/.local/share),
# so no root is needed; `uninstall.sh` reverses it.
set -eu

APP_ID=dev.moonlightcode.MoonlightCode
here=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
data_home=${XDG_DATA_HOME:-$HOME/.local/share}

# Runs from two layouts: this repo (packaging/linux/…) and the release tarball,
# where the binary, icon and this script are unpacked side by side.
if [ -f "$here/icon.png" ]; then
    src_icon=$here/icon.png
else
    src_icon=$(CDPATH= cd -- "$here/../.." && pwd)/icon.png
fi
src_root=$(dirname -- "$src_icon")

# Resolve the binary this entry should launch. An installed entry outlives the
# shell that created it, so the path is baked in rather than resolved at run time.
bin=${1:-}
if [ -z "$bin" ]; then
    bin=$(command -v moonlight 2>/dev/null || true)
fi
for candidate in \
    "$here/moonlight" \
    "$src_root/target/release/moonlight" \
    "$src_root/target/debug/moonlight"; do
    [ -n "$bin" ] && break
    [ -x "$candidate" ] && bin=$candidate
done
if [ -z "$bin" ] || [ ! -x "$bin" ]; then
    echo "install.sh: no moonlight binary found — pass one as the first argument," >&2
    echo "            or run 'cargo build --release -p moonlight-desktop' first." >&2
    exit 1
fi
bin=$(CDPATH= cd -- "$(dirname -- "$bin")" && pwd)/$(basename -- "$bin")

icon_dir=$data_home/$APP_ID
icon=$icon_dir/icon.png
entry_dir=$data_home/applications
entry=$entry_dir/$APP_ID.desktop

mkdir -p "$icon_dir" "$entry_dir"
cp "$src_icon" "$icon"

# `sed` rather than a heredoc so the entry stays a reviewable file in the repo.
sed -e "s|@EXEC@|$bin|" -e "s|@ICON@|$icon|" "$here/$APP_ID.desktop" >"$entry"
chmod 644 "$entry"

# Refresh the desktop's cache where the tool exists; harmless if it doesn't.
if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database "$entry_dir" >/dev/null 2>&1 || true
fi

echo "installed $entry"
echo "          exec = $bin"
echo "          icon = $icon"
echo
echo "The running window is matched to this entry by StartupWMClass=$APP_ID."
echo "If a window was already open, restart it to pick up the association."
