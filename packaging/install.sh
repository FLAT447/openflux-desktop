#!/bin/sh
# OpenFlux Linux installer. Installs the binaries system-wide and registers the desktop
# integration. File capabilities (cap_net_admin for TUN) require a non-nosuid location,
# so the engine goes to /usr/local/lib/openflux rather than staying in the unpacked dir.
set -e

# Directory this script was unpacked into (also holds openflux-gui, openflux, openflux-engine).
SRC=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
PREFIX=/usr/local

# Per-user scheme handler registration first (needs no root; the desktop MimeType entry
# registered below covers fresh logins anyway).
xdg-mime default openflux.desktop x-scheme-handler/openflux 2>/dev/null || true

if [ "$(id -u)" -ne 0 ]; then
    exec pkexec "$0" --internal
fi

# Ensure the runtime dependencies are present before copying the binaries. Optional; you
# can skip this and run packaging/pre_install.sh yourself (e.g. on a distro the detector
# does not recognize).
if [ -x "$SRC/pre_install.sh" ]; then
    echo "==> ensuring runtime dependencies…"
    "$SRC/pre_install.sh" || echo "warning: pre_install.sh failed (distro deps may need manual install)"
fi

# --- root part (pkexec) ----
mkdir -p "$PREFIX/lib/openflux" "$PREFIX/bin"
cp "$SRC/openflux-engine" "$PREFIX/lib/openflux/openflux-engine"
chmod +x "$PREFIX/lib/openflux/openflux-engine"
setcap cap_net_admin+ep "$PREFIX/lib/openflux/openflux-engine" || echo "warning: setcap failed (is the target on a nosuid mount?)"

cp "$SRC/openflux" "$SRC/openflux-gui" "$PREFIX/bin/"
chmod +x "$PREFIX/bin/openflux" "$PREFIX/bin/openflux-gui"

mkdir -p /usr/share/icons/hicolor/512x512/apps /usr/share/applications
if [ -f "$SRC/icon.png" ]; then
    cp "$SRC/icon.png" /usr/share/icons/hicolor/512x512/apps/openflux.png
else
    cp /usr/share/icons/hicolor/512x512/apps/openflux.png "$SRC/icon.png" 2>/dev/null || true
fi
cp "$SRC/openflux.desktop" /usr/share/applications/openflux.desktop
update-desktop-database /usr/share/applications 2>/dev/null || true

echo "OpenFlux installed."
echo "  $(basename "$SRC/openflux-gui") -> $PREFIX/bin/openflux-gui"
echo "  engine                      -> $PREFIX/lib/openflux/openflux-engine (cap_net_admin+ep)"
echo "  launcher + openflux://      -> /usr/share/applications/openflux.desktop"
echo "Run: openflux-gui"