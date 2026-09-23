#!/bin/sh
# OpenFlux Linux pre-install: install the RUNTIME dependencies the application needs to
# actually run (NOT the build toolchain). Detects the distro and uses its package manager.
#
#   ./pre_install.sh          # everything: GUI + TUN + proxy runtime deps
#   ./pre_install.sh --cli    # only CLI/TUI runtime deps (no Tauri GUI libraries)
#
# Runs as root (or restarts itself via sudo / pkexec). Idempotent.
set -eu

install_gui=1
for arg in "$@"; do
    case "$arg" in
        --cli) install_gui=0 ;;
        -h|--help)
            grep -E '^#' "$0" | head -20; exit 0 ;;
    esac
done

if [ "$(id -u)" -ne 0 ]; then
    if command -v sudo >/dev/null 2>&1; then
        exec sudo "$0" "$@"
    elif command -v pkexec >/dev/null 2>&1; then
        exec pkexec "$0" -- "$@"
    else
        echo "pre_install.sh needs root: run it with sudo" >&2
        exit 1
    fi
fi

# --- distro detection ---------------------------------------------------------
FAMILY=
if [ -r /etc/os-release ]; then
    # shellcheck disable=SC1091
    . /etc/os-release
    case "$ID" in
        debian|ubuntu|linuxmint|pop|elementary|neon) FAMILY=debian ;;
        fedora|rhel|centos|rocky|alma|ol|amzn)        FAMILY=redhat ;;
        arch|manjaro|endeavouros|garuda|cachyos)      FAMILY=arch ;;
        opensuse*|sles)                               FAMILY=suse ;;
        alpine)                                       FAMILY=alpine ;;
    esac
fi
[ -z "$FAMILY" ] && echo "unsupported distro (ID=$ID); install the deps for your package manager manually." && exit 1

# --- installer helpers ---------------------------------------------------------
apk_has()  { apk info -e "$1" >/dev/null 2>&1; }
apt_has()  { apt-cache show "$1" >/dev/null 2>&1; }
dnf_has()  { dnf list --available "$1" >/dev/null 2>&1; }
pac_has()  { pacman -Ss "^$1$" >/dev/null 2>&1; }
zypp_has() { zypper info "$1" >/dev/null 2>&1; }

install_pkg() {
    pkg=$1
    case "$FAMILY" in
        debian)  apt_has  "$pkg" && apt-get install -y --no-install-recommends "$pkg" ;;
        redhat)  dnf_has  "$pkg" && dnf install -y "$pkg" ;;
        arch)    pac_has  "$pkg" && pacman -S --noconfirm --needed "$pkg" ;;
        suse)    zypp_has "$pkg" && zypper --non-interactive install "$pkg" ;;
        alpine)  apk_has  "$pkg" && apk add --no-cache "$pkg" ;;
    esac
}

# --- package sets --------------------------------------------------------------
# Core: CLI/TUI, TUN mode (iprule pkexec iproute2), system proxy (gsettings/dbus).
CORE="$(true
    case "$FAMILY" in
        debian) echo iproute2 dbus ca-certificates libglib2.0-bin policykit-1 polkitd-pkexec ;;
        redhat) echo iproute dbus dbus-tools ca-certificates glib2 polkit ;;
        arch)   echo iproute2 dbus ca-certificates glib2 polkit ;;
        suse)   echo iproute2 dbus-1 dbus-1-utils ca-certificates glib2 polkit ;;
        alpine) echo iproute2 dbus ca-certificates glib polkit ;;
    esac)"

# GUI: Tauri v2 webkit runtime + GTK3 + tray (appindicator) + SVG loading.
GUI="$(true
    case "$FAMILY" in
        debian) echo libwebkit2gtk-4.1-0 libgtk-3-0 librsvg2-2 libayatana-appindicator3-1 ;;
        redhat) echo webkit2gtk4.1 gtk3 librsvg2 libappindicator-gtk3 ;;
        arch)   echo webkit2gtk-4.1 gtk3 librsvg libappindicator-gtk3 ;;
        suse)   echo webkit2gtk4-0 gtk3 librsvg2 libappindicator-gtk3 ;;
        alpine) echo webkit2gtk gtk3 librsvg libappindicator-gtk3 ;;
    esac)"

# --- install --------------------------------------------------------------------
echo "==> distro: $ID (family $FAMILY)"
echo "==> installing OpenFlux runtime dependencies…"
for pkg in $CORE; do
    install_pkg "$pkg" || true
done
if [ "$install_gui" -eq 1 ]; then
    echo "==> GUI runtime (skip with --cli)…"
    for pkg in $GUI; do
        install_pkg "$pkg" || true
    done
else
    echo "==> skipping GUI libraries (--cli)"
fi

echo
echo "OpenFlux runtime deps installed for $ID."
[ "$install_gui" -eq 1 ] || echo "run 'sh pre_install.sh' (no --cli) when you later want the GUI."
echo "next: install the bundle binaries with ./install.sh"