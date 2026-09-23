#!/bin/sh
# OpenFlux Linux uninstaller.
set -e

PREFIX=/usr/local

if [ "$(id -u)" -ne 0 ]; then
    exec pkexec "$0"
fi

rm -f "$PREFIX/bin/openflux" "$PREFIX/bin/openflux-gui"
rm -f "$PREFIX/lib/openflux/openflux-engine"
rmdir "$PREFIX/lib/openflux" 2>/dev/null || true
rm -f /usr/share/icons/hicolor/512x512/apps/openflux.png
rm -f /usr/share/applications/openflux.desktop
update-desktop-database /usr/share/applications 2>/dev/null || true

echo "OpenFlux uninstalled (user config under ~/.config/openflux was kept)."