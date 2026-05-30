#!/usr/bin/env bash
set -euo pipefail

REPO="X0RA/Slint_PutMPV"
BINARY_NAME="putmpv-linux-x86_64"
APP_BIN="putmpv"
APP_NAME="PutMPV"
BINARY_URL="https://github.com/${REPO}/releases/latest/download/${BINARY_NAME}"
ICON_URL="https://raw.githubusercontent.com/${REPO}/main/ui/assets/appicon.png"

PREFIX="/usr"
BIN_DEST="${PREFIX}/bin/${APP_BIN}"
DESKTOP_DEST="${PREFIX}/share/applications/putmpv.desktop"
ICON_DEST="${PREFIX}/share/icons/hicolor/256x256/apps/putmpv.png"

if [ "$(uname -s)" != "Linux" ]; then
    echo "install-linux.sh: this installer only runs on Linux." >&2
    exit 1
fi

ARCH="$(uname -m)"
case "${ARCH}" in
    x86_64 | amd64)
        ;;
    *)
        echo "install-linux.sh: only Linux x86_64 builds are published." >&2
        echo "  detected arch: ${ARCH}" >&2
        exit 1
        ;;
esac

if ! command -v curl >/dev/null 2>&1; then
    echo "install-linux.sh: curl is required." >&2
    exit 1
fi

if ! command -v sudo >/dev/null 2>&1 && [ ! -w "${PREFIX}" ]; then
    echo "install-linux.sh: sudo is required to install to ${PREFIX}." >&2
    exit 1
fi

SUDO=()
if [ ! -w "${PREFIX}" ]; then
    SUDO=(sudo)
    echo "${PREFIX} is not writable; sudo will be used."
fi

WORK_DIR="$(mktemp -d -t putmpv-install.XXXXXX)"
cleanup() {
    rm -rf "${WORK_DIR}"
}
trap cleanup EXIT

BINARY_PATH="${WORK_DIR}/${BINARY_NAME}"
ICON_PATH="${WORK_DIR}/putmpv.png"
DESKTOP_PATH="${WORK_DIR}/putmpv.desktop"

echo "Downloading ${BINARY_NAME}..."
curl -fL --progress-bar -o "${BINARY_PATH}" "${BINARY_URL}"
chmod +x "${BINARY_PATH}"

echo "Downloading application icon..."
curl -fL --progress-bar -o "${ICON_PATH}" "${ICON_URL}"

cat >"${DESKTOP_PATH}" <<EOF
[Desktop Entry]
Name=${APP_NAME}
Comment=Browse Put.io media and play with MPV
Exec=${APP_BIN}
Icon=putmpv
Type=Application
Categories=AudioVideo;Video;
Keywords=putio;mpv;media;video;
EOF

warn_if_libmpv_missing() {
    if command -v ldconfig >/dev/null 2>&1 && ldconfig -p 2>/dev/null | grep -q 'libmpv\.so'; then
        return
    fi

    if command -v pkg-config >/dev/null 2>&1 && pkg-config --exists mpv 2>/dev/null; then
        return
    fi

    echo "Warning: libmpv was not detected. PutMPV may not launch until it is installed." >&2
    echo "  Debian/Ubuntu: sudo apt install libmpv2" >&2
    echo "  Fedora: sudo dnf install mpv-libs" >&2
    echo "  Arch: sudo pacman -S mpv" >&2
}

warn_if_libmpv_missing

echo "Installing binary to ${BIN_DEST}..."
"${SUDO[@]}" install -Dm755 "${BINARY_PATH}" "${BIN_DEST}"

echo "Installing desktop launcher to ${DESKTOP_DEST}..."
"${SUDO[@]}" install -Dm644 "${DESKTOP_PATH}" "${DESKTOP_DEST}"

echo "Installing icon to ${ICON_DEST}..."
"${SUDO[@]}" install -Dm644 "${ICON_PATH}" "${ICON_DEST}"

if command -v update-desktop-database >/dev/null 2>&1; then
    "${SUDO[@]}" update-desktop-database "${PREFIX}/share/applications" >/dev/null 2>&1 || true
fi

if command -v gtk-update-icon-cache >/dev/null 2>&1; then
    "${SUDO[@]}" gtk-update-icon-cache -q -t -f "${PREFIX}/share/icons/hicolor" >/dev/null 2>&1 || true
fi

echo "Installed. Run with: ${APP_BIN}"
echo "Desktop launcher: ${APP_NAME}"
