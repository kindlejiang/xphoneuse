#!/usr/bin/env bash
# Build the "XPhoneUse" fork of uxplay (vendor/uxplay) for macOS as a
# UNIVERSAL (arm64 + x86_64) dylib set into src-tauri/resources/.
#
# Run from the repo root in a normal terminal (Homebrew toolchain):
#   scripts/build-uxplay-mac.sh
#
# Two Homebrew prefixes are used, one per architecture:
#   /opt/homebrew  arm64, native
#   /usr/local     x86_64 (installed under Rosetta:
#                  NONINTERACTIVE=1 arch -x86_64 /bin/bash -c "$(curl ...)"
#                  arch -x86_64 /usr/local/bin/brew install glib libplist openssl@3)
# cmake/ninja/pkg-config come from the arm64 prefix (host tools); the x86_64
# pass compiles with -DCMAKE_OSX_ARCHITECTURES=x86_64 and points pkg-config
# and the linker at /usr/local.
#
# Each arch is built and dependency-fixed independently (thin dylibs, all
# references rewritten to @loader_path/<name>), then lipo-merged into fat
# dylibs in src-tauri/resources/. The set is self-contained and loads
# identically in dev (target/debug/resources/, all files copied together by
# tauri dev) and in a bundled .app (Contents/Resources/resources/), on both
# Intel and Apple Silicon Macs.
#
# (bash 3.2 compatible: no associative arrays; fixup state lives in the
# globals FIX_PREFIX / FIX_DEP_LIST, valid because fixup_arch runs
# sequentially, never nested.)
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
UXPLAY_DIR="$REPO_ROOT/vendor/uxplay"
OUT_DIR="$REPO_ROOT/src-tauri/resources"
BREW_ARM="$(brew --prefix)"   # /opt/homebrew
BREW_X86="/usr/local"

echo "==> Checking dependencies"
command -v cmake >/dev/null || { echo "cmake not found; run: brew install cmake"; exit 1; }
command -v ninja >/dev/null || { echo "ninja not found; run: brew install ninja"; exit 1; }
pkg-config --exists glib-2.0 || { echo "glib not found; run: brew install glib"; exit 1; }
pkg-config --exists libplist-2.0 || { echo "libplist not found; run: brew install libplist"; exit 1; }
pkg-config --exists openssl || { echo "openssl not found; run: brew install openssl@3"; exit 1; }
for f in glib libplist openssl@3; do
    [ -d "$BREW_X86/opt/$f" ] || {
        echo "x86_64 Homebrew package $f missing; run:"
        echo "  arch -x86_64 $BREW_X86/bin/brew install glib libplist openssl@3"
        exit 1
    }
done

command -v lipo >/dev/null || { echo "lipo not found"; exit 1; }

# ---------------------------------------------------------------------------
# Per-arch helpers
# ---------------------------------------------------------------------------

# build_arch <arch> <build_dir>: cmake configure + build.
build_arch() {
    local arch="$1" bdir="$2"
    echo "==> Building uxplay ($arch) in $bdir"
    if [ "$arch" = "arm64" ]; then
        cmake -S "$UXPLAY_DIR" -B "$bdir" -G Ninja -DNO_MARCH_NATIVE=ON \
            -DCMAKE_EXE_LINKER_FLAGS="-L$BREW_ARM/lib" \
            -DCMAKE_SHARED_LINKER_FLAGS="-L$BREW_ARM/lib"
    else
        # x86_64: cross-compile with Xcode clang, resolve everything from
        # the /usr/local (x86_64 Homebrew) prefix.
        env PKG_CONFIG_PATH="$BREW_X86/lib/pkgconfig:$BREW_X86/opt/openssl@3/lib/pkgconfig" \
        cmake -S "$UXPLAY_DIR" -B "$bdir" -G Ninja -DNO_MARCH_NATIVE=ON \
            -DCMAKE_OSX_ARCHITECTURES=x86_64 \
            -DCMAKE_PREFIX_PATH="$BREW_X86" \
            -DCMAKE_IGNORE_PREFIX_PATH="$BREW_ARM" \
            -DCMAKE_EXE_LINKER_FLAGS="-L$BREW_X86/lib" \
            -DCMAKE_SHARED_LINKER_FLAGS="-L$BREW_X86/lib"
    fi
    cmake --build "$bdir"
}

# scan_deps <file>: append any FIX_PREFIX-referenced dylib of <file> that is
# not yet in FIX_DEP_LIST.
scan_deps() {
    otool -L "$1" | tail -n +2 | awk '{print $1}' | while read -r ref; do
        case "$ref" in
            "$FIX_PREFIX"/*) ;;
            *) continue ;;
        esac
        [ -f "$ref" ] || continue
        if ! grep -qxF "$ref" "$FIX_DEP_LIST"; then
            echo "$ref" >> "$FIX_DEP_LIST"
        fi
    done
}

# rewrite_refs <file>: rewrite every FIX_PREFIX reference to
# @loader_path/<basename>; ad-hoc re-sign afterwards (install_name_tool
# invalidates the signature, and arm64 enforces it at load).
rewrite_refs() {
    local f="$1"
    local fname; fname="$(basename "$f")"
    local changed=0
    while read -r ref; do
        case "$ref" in
            "$FIX_PREFIX"/*) ;;
            *) continue ;;
        esac
        local name; name="$(basename "$ref")"
        if [ "$name" = "$fname" ]; then
            # the dylib's own install id
            install_name_tool -id "@loader_path/$name" "$f"
        else
            install_name_tool -change "$ref" "@loader_path/$name" "$f"
        fi
        changed=1
    done < <(otool -L "$f" | tail -n +2 | awk '{print $1}')
    if [ "$changed" -eq 1 ]; then
        codesign --force --sign - "$f" >/dev/null 2>&1
    fi
}

# fixup_arch <brew_prefix> <build_dir> <stage_dir>: copy the freshly built
# thin uxplay.dylib into stage_dir, collect the transitive closure of
# Homebrew-referenced dylibs (fixpoint scan), copy them in, rewrite refs.
fixup_arch() {
    local prefix="$1" bdir="$2" stage="$3"
    mkdir -p "$stage"
    cp "$bdir/uxplay.dylib" "$stage/uxplay.dylib"

    FIX_PREFIX="$prefix"
    FIX_DEP_LIST="$(mktemp)"

    : > "$FIX_DEP_LIST"
    local prev=-1
    while [ "$(wc -l < "$FIX_DEP_LIST" | tr -d ' ')" != "$prev" ]; do
        prev="$(wc -l < "$FIX_DEP_LIST" | tr -d ' ')"
        scan_deps "$stage/uxplay.dylib"
        while read -r ref; do
            scan_deps "$ref"
        done < "$FIX_DEP_LIST"
    done

    while read -r ref; do
        cp "$ref" "$stage/"
    done < "$FIX_DEP_LIST"

    rewrite_refs "$stage/uxplay.dylib"
    while read -r ref; do
        rewrite_refs "$stage/$(basename "$ref")"
    done < "$FIX_DEP_LIST"

    rm -f "$FIX_DEP_LIST"
    echo "==> [$prefix] fixed up $(ls "$stage" | wc -l | tr -d ' ') dylibs"
}

# ---------------------------------------------------------------------------
# Build + fixup both arches, then lipo-merge
# ---------------------------------------------------------------------------

build_arch arm64 "$UXPLAY_DIR/build-mac-arm64"
build_arch x86_64 "$UXPLAY_DIR/build-mac-x86_64"

STAGE_ARM="$(mktemp -d)"
STAGE_X86="$(mktemp -d)"
fixup_arch "$BREW_ARM" "$UXPLAY_DIR/build-mac-arm64" "$STAGE_ARM"
fixup_arch "$BREW_X86" "$UXPLAY_DIR/build-mac-x86_64" "$STAGE_X86"

echo "==> Lipo-merging into $OUT_DIR"
mkdir -p "$OUT_DIR"
merged=0
for f in "$STAGE_ARM"/*; do
    name="$(basename "$f")"
    if [ ! -f "$STAGE_X86/$name" ]; then
        echo "ERROR: $name exists in the arm64 closure but not x86_64 — closures diverged." >&2
        exit 1
    fi
    lipo -create -output "$OUT_DIR/$name" "$STAGE_ARM/$name" "$STAGE_X86/$name"
    # lipo drops the per-slice ad-hoc signature — re-sign the fat file.
    codesign --force --sign - "$OUT_DIR/$name" >/dev/null 2>&1
    # lipo inherits the read-only source mode; tauri-build copies resources
    # over previous copies and needs them writable.
    chmod 644 "$OUT_DIR/$name"
    merged=$((merged + 1))
done
for f in "$STAGE_X86"/*; do
    name="$(basename "$f")"
    if [ ! -f "$STAGE_ARM/$name" ]; then
        echo "ERROR: $name exists in the x86_64 closure but not arm64 — closures diverged." >&2
        exit 1
    fi
done

rm -rf "$STAGE_ARM" "$STAGE_X86"

echo "==> Done: $OUT_DIR/uxplay.dylib (+ $((merged - 1)) dependency dylibs, universal arm64+x86_64)"
echo "    Verify with:  cargo run -p xphoneuse-ios --example uxplay_smoke"
echo "    (macOS firewall may ask to allow inbound connections for AirPlay.)"
