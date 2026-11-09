#!/usr/bin/env bash
# Build the "XPhoneUse" fork of uxplay (vendor/uxplay) with MinGW-64 and copy
# uxplay.dll plus its MinGW runtime dependencies into the app's resources
# directory.
#
# Run this INSIDE an MSYS2 UCRT64 terminal (MINGW64 works too if you swap
# the package names, see docs/IOS.md). The fork has GStreamer removed and
# ships its own mDNS responder, so the Bonjour SDK is NOT needed (only for
# "-DUSE_DNS_SD=1" builds).
#
#   /ucrt64/bin/bash scripts/build-uxplay.sh
#
# Result: src-tauri/resources/uxplay.dll + dependency DLLs (bundled into
# releases and used in dev by the Tauri app).
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
UXPLAY_DIR="$REPO_ROOT/vendor/uxplay"
OUT_DIR="$REPO_ROOT/src-tauri/resources"

echo "==> Checking dependencies"
command -v cmake >/dev/null || { echo "cmake not found; run: pacman -S mingw-w64-ucrt-x86_64-cmake"; exit 1; }
command -v ninja >/dev/null || { echo "ninja not found; run: pacman -S ninja"; exit 1; }
pkg-config --exists libplist-2.0 || { echo "libplist not found; run: pacman -S mingw-w64-ucrt-x86_64-libplist"; exit 1; }
pkg-config --exists openssl || { echo "openssl not found; run: pacman -S mingw-w64-ucrt-x86_64-openssl"; exit 1; }

echo "==> Building uxplay (exe + dll) in $UXPLAY_DIR/build"
mkdir -p "$UXPLAY_DIR/build"
cd "$UXPLAY_DIR/build"
cmake .. -G Ninja -DNO_MARCH_NATIVE=ON
ninja

echo "==> Copying uxplay.dll and MinGW runtime dependencies to $OUT_DIR"
mkdir -p "$OUT_DIR"
cp uxplay.dll "$OUT_DIR/uxplay.dll"

# Dependency closure of uxplay.dll (see vendor/uxplay/FORK.md). The
# api-ms-win-crt-* and KERNEL32/USER32/WS2_32 DLLs are part of Windows
# and must NOT be copied.
MINGW_BIN="$(pkg-config --variable=prefix glib-2.0)/bin"
for dll in \
    libgcc_s_seh-1.dll libglib-2.0-0.dll libwinpthread-1.dll libstdc++-6.dll \
    libcrypto-3-x64.dll libplist-2.0.dll \
    libintl-8.dll libiconv-2.dll libpcre2-8-0.dll libffi-8.dll zlib1.dll; do
    if [ -f "$MINGW_BIN/$dll" ]; then
        cp "$MINGW_BIN/$dll" "$OUT_DIR/$dll"
    else
        echo "WARNING: $MINGW_BIN/$dll not found; uxplay.dll may fail to load"
    fi
done

echo "==> Done: $OUT_DIR/uxplay.dll"
echo "    Verify with:  cargo run -p xphoneuse-ios --example uxplay_smoke"
echo "    (Windows Firewall must allow inbound connections for AirPlay.)"
