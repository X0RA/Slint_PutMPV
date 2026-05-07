#!/usr/bin/env bash
set -euo pipefail

REPO="X0RA/Slint_PutMPV"
DMG_NAME="putmpv-macos-arm64.dmg"
APP_NAME="PutMPV.app"
APP_DEST="/Applications/${APP_NAME}"
DMG_URL="https://github.com/${REPO}/releases/latest/download/${DMG_NAME}"

if [ "$(uname -s)" != "Darwin" ]; then
    echo "install-macos.sh: this installer only runs on macOS." >&2
    exit 1
fi

if [ "$(uname -m)" != "arm64" ]; then
    echo "install-macos.sh: only Apple Silicon (arm64) builds are published." >&2
    echo "  detected arch: $(uname -m)" >&2
    exit 1
fi

WORK_DIR="$(mktemp -d -t putmpv-install.XXXXXX)"
MOUNT_POINT=""

cleanup() {
    if [ -n "${MOUNT_POINT}" ] && [ -d "${MOUNT_POINT}" ]; then
        hdiutil detach "${MOUNT_POINT}" -quiet >/dev/null 2>&1 || true
    fi
    rm -rf "${WORK_DIR}"
}
trap cleanup EXIT

DMG_PATH="${WORK_DIR}/${DMG_NAME}"
echo "Downloading ${DMG_NAME}..."
curl -fL --progress-bar -o "${DMG_PATH}" "${DMG_URL}"

echo "Mounting DMG..."
MOUNT_POINT="$(hdiutil attach "${DMG_PATH}" -nobrowse -readonly | awk -F '\t' '/\/Volumes\//{print $NF; exit}')"

if [ -z "${MOUNT_POINT}" ] || [ ! -d "${MOUNT_POINT}/${APP_NAME}" ]; then
    echo "install-macos.sh: failed to locate ${APP_NAME} inside the DMG." >&2
    exit 1
fi

SUDO=""
if [ ! -w "/Applications" ]; then
    SUDO="sudo"
    echo "/Applications is not writable; sudo will be used."
fi

if [ -e "${APP_DEST}" ]; then
    echo "Removing existing ${APP_DEST}..."
    ${SUDO} rm -rf "${APP_DEST}"
fi

echo "Installing to ${APP_DEST}..."
${SUDO} ditto "${MOUNT_POINT}/${APP_NAME}" "${APP_DEST}"

${SUDO} xattr -dr com.apple.quarantine "${APP_DEST}" 2>/dev/null || true

echo "Installed. Open with: open \"${APP_DEST}\""
