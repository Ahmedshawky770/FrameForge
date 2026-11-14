# FrameForge AppImage Build Script
# Builds a portable AppImage for Linux distributions
#
# Prerequisites:
#   - appimagetool (https://github.com/AppImage/AppImageKit/releases)
#   - cargo (Rust toolchain)
#   - Built frameforge-tauri-app binary in target/release/
#
# Usage:
#   ./build-appimage.sh

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_DIR="$(cd "${SCRIPT_DIR}/.." && pwd)"
APPDIR="${PROJECT_DIR}/packaging/appimage"
OUTPUT="${PROJECT_DIR}/packaging/FrameForge-0.1.0-x86_64.AppImage"

echo "=== FrameForge AppImage Builder ==="
echo "Project dir: ${PROJECT_DIR}"
echo "AppDir: ${APPDIR}"
echo "Output: ${OUTPUT}"
echo ""

# Step 1: Build the Rust application in release mode
echo "[1/4] Building Rust application..."
cd "${PROJECT_DIR}"
cargo build --release --locked --bin frameforge-tauri-app

# Step 2: Copy the binary and assets into AppDir
echo "[2/4] Populating AppDir..."
mkdir -p "${APPDIR}/usr/bin"
mkdir -p "${APPDIR}/usr/share/applications"
mkdir -p "${APPDIR}/usr/share/icons/hicolor/256x256/apps"
mkdir -p "${APPDIR}/usr/lib"

cp "${PROJECT_DIR}/target/release/frameforge-tauri-app" "${APPDIR}/usr/bin/frameforge"
chmod +x "${APPDIR}/usr/bin/frameforge"

cp "${SCRIPT_DIR}/AppRun" "${APPDIR}/AppRun"
chmod +x "${APPDIR}/AppRun"

cp "${PROJECT_DIR}/packaging/debian/frameforge.desktop" "${APPDIR}/usr/share/applications/frameforge.desktop"

# Copy icon (placeholder - replace with real icon)
if [ -f "${PROJECT_DIR}/apps/tauri-app/icons/icon.png" ]; then
    cp "${PROJECT_DIR}/apps/tauri-app/icons/icon.png" "${APPDIR}/usr/share/icons/hicolor/256x256/apps/frameforge.png"
fi

# Step 3: Bundle runtime dependencies
echo "[3/4] Bundling runtime dependencies..."
# Use ldd to find required libraries and copy them
# Note: This is a simplified approach. For production, use linuxdeployqt or similar
# to properly bundle all GTK/WebKitGTK dependencies.

# Step 4: Build the AppImage
echo "[4/4] Building AppImage..."
if command -v appimagetool &> /dev/null; then
    export ARCH=x86_64
    appimagetool "${APPDIR}" "${OUTPUT}"
    echo ""
    echo "=== AppImage built successfully ==="
    echo "Output: ${OUTPUT}"
    echo ""
    echo "IMPORTANT NOTES:"
    echo "  - This AppImage bundles the application binary but relies on host"
    echo "    system libraries (GTK, WebKitGTK, libmpv, FFmpeg)."
    echo "  - Hardware acceleration (VAAPI/NVENC) requires host drivers."
    echo "  - Test on Pop!_OS 22.04/24.04 before distribution."
else
    echo "ERROR: appimagetool not found."
    echo "Install it from: https://github.com/AppImage/AppImageKit/releases"
    echo ""
    echo "You can manually create the AppImage by running:"
    echo "  export ARCH=x86_64"
    echo "  appimagetool ${APPDIR} ${OUTPUT}"
    exit 1
fi
