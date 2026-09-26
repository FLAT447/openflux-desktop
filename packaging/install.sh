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
LIBDIR=$PREFIX/lib/openflux
BINDIR=$PREFIX/bin
mkdir -p "$LIBDIR" "$BINDIR"

# Updating a build that is currently running used to die with
#   cp: cannot create regular file '/usr/local/lib/openflux/openflux-engine': Text file busy
# because cp(1) writes into the existing inode, and the kernel refuses to open a running
# executable for writing (ETXTBSY). Stage the new file next to the target and rename it into
# place instead: rename(2) only swaps the directory entry, so it succeeds while the old
# inode keeps running. The old process still needs a restart to execute the new build, which
# the summary at the end points out.
#
# $3, when set, is applied to the staged file before the rename (setcap writes an xattr,
# and xattrs survive rename inside the same directory).
CAP_APPLIED=0
install_binary() {
    _src=$1
    _dst=$2
    _cap=$3
    _tmp=$_dst.new.$$
    if ! cp -- "$_src" "$_tmp"; then
        rm -f "$_tmp"
        echo "error: cannot copy $_src (check free space in $(dirname "$_dst"))" >&2
        return 1
    fi
    if ! chmod 0755 "$_tmp"; then
        rm -f "$_tmp"
        echo "error: cannot make $_dst executable" >&2
        return 1
    fi
    if [ -n "$_cap" ]; then
        if [ -z "$SETCAP" ]; then
            echo "warning: setcap not found (install libcap); $_dst gets no capability and TUN mode cannot work"
        elif "$SETCAP" "$_cap" "$_tmp"; then
            CAP_APPLIED=1
        else
            echo "warning: setcap $_cap failed (is $_dst on a nosuid mount?)"
        fi
    fi
    if ! mv -f -- "$_tmp" "$_dst"; then
        rm -f "$_tmp"
        echo "error: cannot install $_dst" >&2
        return 1
    fi
}

# pkexec trims PATH (the same reason the GUI hardcodes /usr/sbin for `ip`), so resolve
# setcap by absolute path instead of trusting PATH: a silently missing setcap leaves the
# engine without cap_net_admin, and the only symptom is a TUN tunnel that never connects.
SETCAP=$(command -v setcap 2>/dev/null || true)
GETCAP=$(command -v getcap 2>/dev/null || true)
for _c in /sbin/setcap /usr/sbin/setcap; do
    [ -n "$SETCAP" ] && break
    [ -x "$_c" ] && SETCAP=$_c
done
for _c in /sbin/getcap /usr/sbin/getcap; do
    [ -n "$GETCAP" ] && break
    [ -x "$_c" ] && GETCAP=$_c
done

install_binary "$SRC/openflux-engine" "$LIBDIR/openflux-engine" cap_net_admin+ep
install_binary "$SRC/openflux" "$BINDIR/openflux"
install_binary "$SRC/openflux-gui" "$BINDIR/openflux-gui"

mkdir -p /usr/share/icons/hicolor/512x512/apps /usr/share/applications
if [ -f "$SRC/icon.png" ]; then
    cp "$SRC/icon.png" /usr/share/icons/hicolor/512x512/apps/openflux.png
else
    cp /usr/share/icons/hicolor/512x512/apps/openflux.png "$SRC/icon.png" 2>/dev/null || true
fi
cp "$SRC/openflux.desktop" /usr/share/applications/openflux.desktop
update-desktop-database /usr/share/applications 2>/dev/null || true

# Confirm the capability actually landed on the installed file (getcap is the portable way
# to read it back; ALT's setcap has no -p). Without it TUN cannot mark its own sockets.
if [ "$CAP_APPLIED" = 1 ] && { [ -z "$GETCAP" ] || "$GETCAP" "$LIBDIR/openflux-engine" 2>/dev/null | grep -q cap_net_admin; }; then
    ENGINE_CAP="cap_net_admin=ep"
else
    ENGINE_CAP="NO capability - TUN mode will not work, see the warnings above"
fi

echo "OpenFlux installed."
echo "  $(basename "$SRC/openflux-gui") -> $BINDIR/openflux-gui"
echo "  engine                      -> $LIBDIR/openflux-engine ($ENGINE_CAP)"
echo "  launcher + openflux://      -> /usr/share/applications/openflux.desktop"

# Replacing the binary does not replace the running process: an already-open window keeps
# executing the previous build (and its previous config schema), so the fix that was just
# installed appears to have "no effect". Point that out instead of killing the user's window.
if pgrep -x "$(basename "$SRC/openflux-gui")" >/dev/null 2>&1; then
    echo
    echo "warning: an OpenFlux GUI process is still running the previous build."
    echo "         Close it and start openflux-gui again, otherwise the update is not picked up."
fi
echo "Run: openflux-gui"