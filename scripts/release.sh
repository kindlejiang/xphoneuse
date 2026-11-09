#!/usr/bin/env bash
# Build the installers for THIS machine's platform and publish them to
# CrabNebula Cloud (https://crabnebula.cloud) — the draft → upload →
# publish pipeline used by https://web.crabnebula.cloud/crabnebula/cn-cli/releases.
#
# Usage (from the repo root):
#   scripts/release.sh [--app org/app] [--notes "..." | --notes-file f]
#                      [--arch universal|arm64|x86_64]  (macOS only, default universal)
#                      [--channel beta] [--skip-build] [--only-build]
#                      [--no-publish] [--publish]
#
# Environment:
#   CN_API_KEY                    required to publish (create in web.crabnebula.cloud;
#                                 alternatively stored in ~/.crabnebula/apikey)
#   CN_APP                        default for --app (e.g. "aovik/xphoneuse")
#   TAURI_SIGNING_PRIVATE_KEY     updater signing key (tauri.key). Without it the
#   [TAURI_SIGNING_PRIVATE_KEY_PASSWORD]  build skips updater artifacts — the release
#                                 is install-only, auto-update won't find it.
#   APPLE_SIGNING_IDENTITY        macOS cert ("Developer ID Application: ...").
#                                 Without it the app is ad-hoc signed ("-", Gatekeeper
#                                 warns on first launch) and not notarized.
#   CN_BIN                        path to the cn CLI (downloaded automatically to
#                                 ~/.crabnebula/bin/cn if not on PATH)
#   TAURI_BUNDLES                 override bundle types (macOS default "dmg,app"
#                                 — the app target carries the updater artifact,
#                                 Windows "nsis", Linux "deb,appimage")
#
# One platform per machine (Tauri bundles can only be built on their own
# OS): run this script with --no-publish on each machine (macOS, then
# Windows/Linux in Git Bash / a shell) with the SAME version — uploads
# append to the same draft release. Publish once at the end from any
# machine with `--publish`, or click "Publish draft" in the web UI.
#
# (bash 3.2 compatible: no associative arrays.)
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
cd "$REPO_ROOT"

# python3 on unix, python/py on Windows Git Bash. Probe each candidate with
# -c: on Windows the Microsoft Store "python3" stub exists on PATH but exits
# 49 with no output, and `command -v` can't tell the difference.
PY=""
for c in python3 python py; do
    p="$(command -v "$c" 2>/dev/null || true)"
    if [ -n "$p" ] && "$p" -c "pass" >/dev/null 2>&1; then
        PY="$p"
        break
    fi
done
[ -n "$PY" ] || { echo "ERROR: python not found" >&2; exit 1; }

# Default app slug from the updater endpoint in tauri.conf.json
# (https://cdn.crabnebula.app/update/<org>/<app>/...).
APP="${CN_APP:-}"
[ -n "$APP" ] || APP="$("$PY" - <<'PYEOF'
import json, re
cfg = json.load(open("src-tauri/tauri.conf.json"))
eps = cfg.get("plugins", {}).get("updater", {}).get("endpoints", [])
for e in eps:
    m = re.search(r"crabnebula\.app/update/([^/]+)/([^/]+)/", e)
    if m:
        print(f"{m.group(1)}/{m.group(2)}")
        break
PYEOF
)"
VERSION=""
NOTES=""
NOTES_FILE=""
CHANNEL=""
CHANNEL_ARGS=()
ARCH="universal"
DO_BUILD=1
DO_PUBLISH=1
DO_UPLOAD=1

usage() { sed -n '2,33p' "$0" | sed 's/^# \{0,1\}//'; exit "${1:-0}"; }

while [ $# -gt 0 ]; do
    case "$1" in
        --app) APP="$2"; shift 2 ;;
        --version) VERSION="$2"; shift 2 ;;
        --notes) NOTES="$2"; shift 2 ;;
        --notes-file) NOTES_FILE="$2"; shift 2 ;;
        --channel) CHANNEL="$2"; CHANNEL_ARGS=(--channel "$CHANNEL"); shift 2 ;;
        --arch) ARCH="$2"; shift 2 ;;
        --skip-build) DO_BUILD=0; shift ;;
        --only-build) DO_BUILD=1; DO_PUBLISH=0; DO_UPLOAD=0; shift ;;
        --no-publish) DO_PUBLISH=0; shift ;;  # build + upload, keep draft unpublished
        --publish) DO_BUILD=0; DO_UPLOAD=0; shift ;;  # publish an existing draft only
        -h|--help) usage 0 ;;
        *) echo "unknown option: $1" >&2; usage 1 ;;
    esac
done

# ---------------------------------------------------------------------------
# Platform + toolchain checks
# ---------------------------------------------------------------------------
case "$(uname -s)" in
    Darwin)  PLATFORM="macos" ;;
    Linux)   PLATFORM="linux" ;;
    MINGW*|MSYS*|CYGWIN*) PLATFORM="windows" ;;
    *) echo "ERROR: unsupported platform $(uname -s)" >&2; exit 1 ;;
esac

command -v bun >/dev/null || { echo "ERROR: bun not found" >&2; exit 1; }

[ -n "$VERSION" ] || VERSION="$("$PY" -c 'import json;print(json.load(open("src-tauri/tauri.conf.json"))["version"])')"
echo "==> Releasing XPhoneUse v$VERSION ($PLATFORM)"

if [ "$PLATFORM" = "macos" ] && [ "$ARCH" = "universal" ]; then
    rustup target list --installed 2>/dev/null | grep -q x86_64-apple-darwin \
        || { echo "ERROR: rustup target x86_64-apple-darwin missing; run: rustup target add x86_64-apple-darwin" >&2; exit 1; }
fi

# ---------------------------------------------------------------------------
# Build
# ---------------------------------------------------------------------------
# Non-default targets change the output directory. Computed before the build
# guard so --skip-build upload points at the same layout the build used.
TARGET_ARGS=()
BUNDLE_DIR="src-tauri/target/release/bundle"
if [ "$PLATFORM" = "macos" ]; then
    case "$ARCH" in
        universal) TARGET_ARGS=(--target universal-apple-darwin); BUNDLE_DIR="src-tauri/target/universal-apple-darwin/release/bundle" ;;
        arm64) ;;   # host arch, default target dir
        x86_64) TARGET_ARGS=(--target x86_64-apple-darwin); BUNDLE_DIR="src-tauri/target/x86_64-apple-darwin/release/bundle" ;;
        *) echo "ERROR: --arch must be universal|arm64|x86_64 (macOS only)" >&2; exit 1 ;;
    esac
fi

if [ "$DO_BUILD" -eq 1 ]; then
    # Bundle types per platform. macOS needs BOTH: updater artifacts
    # (.app.tar.gz + .sig) are only produced for the `app` target, the dmg
    # alone carries none (tauri-bundler excludes dmg from updater targets).
    DEFAULT_BUNDLES="dmg,app";  [ "$PLATFORM" = "windows" ] && DEFAULT_BUNDLES="nsis"
    [ "$PLATFORM" = "linux" ] && DEFAULT_BUNDLES="deb,appimage"
    BUNDLES="${TAURI_BUNDLES:-$DEFAULT_BUNDLES}"

    # Without the updater key skip updater artifacts; without a Developer ID
    # cert fall back to ad-hoc signing. TAURI_SIGNING_PRIVATE_KEY accepts a
    # path or the key content; default to the repo's tauri.key (gitignored)
    # so only the password needs to come from the environment.
    if [ -z "${TAURI_SIGNING_PRIVATE_KEY:-}" ] && [ -f "$REPO_ROOT/tauri.key" ]; then
        export TAURI_SIGNING_PRIVATE_KEY="$REPO_ROOT/tauri.key"
    fi
    INNER=""
    if [ -z "${TAURI_SIGNING_PRIVATE_KEY:-}" ]; then
        echo "    (no TAURI_SIGNING_PRIVATE_KEY → updater artifacts skipped: release is install-only)"
        INNER='"createUpdaterArtifacts":false'
    elif [ -z "${TAURI_SIGNING_PRIVATE_KEY_PASSWORD:-}" ]; then
        echo "    hint: key found; export TAURI_SIGNING_PRIVATE_KEY_PASSWORD if the key is encrypted"
    fi
    if [ "$PLATFORM" = "macos" ] && [ -z "${APPLE_SIGNING_IDENTITY:-}" ]; then
        echo "    (no APPLE_SIGNING_IDENTITY → ad-hoc signed, not notarized)"
        MAC='"macOS":{"signingIdentity":"-"}'
        if [ -n "$INNER" ]; then INNER="$INNER,$MAC"; else INNER="$MAC"; fi
    fi
    CONFIG_ARGS=()
    if [ -n "$INNER" ]; then
        CONFIG_ARGS=(--config "{\"bundle\":{$INNER}}")
    fi

    # The `cn release upload --framework tauri` discovery uploads EVERYTHING
    # under the bundle dir — clear stale bundles from older versions first.
    rm -rf "$BUNDLE_DIR"

    # tauri-build copies resources into target/<triple>/release/resources/
    # and fails with EACCES when a previous copy is read-only (older
    # build-uxplay-mac.sh outputs were mode 444).
    find src-tauri/target -path '*/release/resources/*' -type f ! -perm -u+w \
        -exec chmod u+w {} + 2>/dev/null || true

    # dist/ is embedded at compile time and not in the cargo fingerprint:
    # force the recompile so a fresh frontend build is actually embedded.
    touch src-tauri/src/lib.rs

    echo "==> Building $PLATFORM installers ($BUNDLES)"
    if [ "$PLATFORM" = "macos" ]; then
        # CI=true skips the Finder AppleScript step of dmg creation, which
        # times out in background/headless shells.
        CI=true bunx tauri build --bundles "$BUNDLES" ${TARGET_ARGS[@]+"${TARGET_ARGS[@]}"} ${CONFIG_ARGS[@]+"${CONFIG_ARGS[@]}"}
    else
        bunx tauri build --bundles "$BUNDLES" ${TARGET_ARGS[@]+"${TARGET_ARGS[@]}"} ${CONFIG_ARGS[@]+"${CONFIG_ARGS[@]}"}
    fi

    echo "==> Bundles:"
    find "$BUNDLE_DIR" -type f \( -name '*.dmg' -o -name '*.exe' -o -name '*.msi' \
        -o -name '*.deb' -o -name '*.AppImage' -o -name '*.rpm' \
        -o -name '*.app.tar.gz' -o -name '*.nsis.zip' -o -name '*.sig' \) -print || true
fi

if [ "$DO_UPLOAD" -eq 0 ] && [ "$DO_PUBLISH" -eq 0 ]; then
    echo "==> build only: skipping CrabNebula Cloud publish"
    exit 0
fi

# ---------------------------------------------------------------------------
# CrabNebula Cloud publish
# ---------------------------------------------------------------------------
[ -n "$APP" ] || { echo "ERROR: no app slug. Pass --app org/app (or export CN_APP)." >&2; exit 1; }
# API key: env → ~/.crabnebula/apikey file (created outside the chat/CI
# logs, e.g. `printf %s cn_xxx > ~/.crabnebula/apikey && chmod 600 ...`).
if [ -z "${CN_API_KEY:-}" ] && [ -f "$HOME/.crabnebula/apikey" ]; then
    export CN_API_KEY="$(tr -d '\n' < "$HOME/.crabnebula/apikey")"
fi
[ -n "${CN_API_KEY:-}" ] || { echo "ERROR: CN_API_KEY not set. Create a key at https://web.crabnebula.cloud, then export it or store it in ~/.crabnebula/apikey." >&2; exit 1; }

# cn CLI: PATH → CN_BIN → auto-download to ~/.crabnebula/bin
CN_DL=""
case "$PLATFORM" in
    macos) CN_DL="https://cdn.crabnebula.app/download/crabnebula/cn-cli/latest/cn_macos"; CN_NAME="cn" ;;
    linux) CN_DL="https://cdn.crabnebula.app/download/crabnebula/cn-cli/latest/cn_linux"; CN_NAME="cn" ;;
    windows) CN_DL="https://cdn.crabnebula.app/download/crabnebula/cn-cli/latest/cn_windows.exe"; CN_NAME="cn.exe" ;;
esac
CN="${CN_BIN:-$(command -v cn 2>/dev/null || true)}"
if [ -z "$CN" ]; then
    CN="$HOME/.crabnebula/bin/$CN_NAME"
    if [ ! -x "$CN" ]; then
        echo "==> Downloading cn CLI to $CN"
        mkdir -p "$(dirname "$CN")"
        curl -sL "$CN_DL" -o "$CN" && chmod +x "$CN"
    fi
fi
"$CN" whoami >/dev/null

if [ "$DO_UPLOAD" -eq 1 ]; then
echo "==> Drafting release $APP v$VERSION"
# Reuse an existing draft for this version if present, so the script can be
# re-run (or run on another platform) without duplicating releases.
if "$CN" release show "$APP" "$VERSION" ${CHANNEL_ARGS[@]+"${CHANNEL_ARGS[@]}"} >/dev/null 2>&1; then
    echo "    release v$VERSION already exists — reusing it"
else
    NOTES_ARGS=()
    if [ -n "$NOTES_FILE" ]; then
        NOTES_ARGS=(--notes-file "$NOTES_FILE")
    elif [ -n "$NOTES" ]; then
        NOTES_ARGS=(--notes "$NOTES")
    else
        # Default notes: commits since the last tag.
        LAST_TAG="$(git describe --tags --abbrev=0 2>/dev/null || true)"
        if [ -n "$LAST_TAG" ]; then
            NOTES_FILE="$(mktemp)"
            git log --oneline "$LAST_TAG..HEAD" > "$NOTES_FILE" || true
            NOTES_ARGS=(--notes-file "$NOTES_FILE")
        fi
    fi
    "$CN" release draft "$APP" "$VERSION" ${NOTES_ARGS[@]+"${NOTES_ARGS[@]}"} ${CHANNEL_ARGS[@]+"${CHANNEL_ARGS[@]}"}
fi

echo "==> Uploading bundles from $BUNDLE_DIR"
# Explicit per-asset uploads (documented CLI flags) instead of
# `--framework tauri` auto-discovery: the discovery shows an interactive
# asset picker (unusable in scripts/CI) and does not recognize tauri's
# `_universal` filenames. Platform names follow the documented tables:
#   public:  dmg-x86_64/dmg-aarch64, nsis-x86_64, appimage-x86_64, deb-x86_64 ...
#   update:  darwin-x86_64/darwin-aarch64, windows-x86_64, linux-x86_64 ...
# A .sig next to the file is picked up automatically.
upload() {  # upload <file> [flags...]
    local f="$1"; shift
    echo "    $(basename "$f")"
    "$CN" release upload "$APP" "$VERSION" --file "$f" ${CHANNEL_ARGS[@]+"${CHANNEL_ARGS[@]}"} "$@"
}

if [ "$PLATFORM" = "macos" ]; then
    DMG="$(ls "$BUNDLE_DIR"/dmg/*.dmg 2>/dev/null | head -1 || true)"
    TAR="$(ls "$BUNDLE_DIR"/macos/*.app.tar.gz 2>/dev/null | head -1 || true)"
    [ -n "$DMG" ] || { echo "ERROR: no dmg found in $BUNDLE_DIR/dmg" >&2; exit 1; }
    if [ "$ARCH" = "universal" ]; then
        # One fat dmg serves both arches. dmg-universal is not in the
        # documented platform list — if the API rejects it, tag the file
        # under both arches instead.
        upload "$DMG" --public-platform dmg-universal || {
            upload "$DMG" --public-platform dmg-aarch64
            upload "$DMG" --public-platform dmg-x86_64
        }
        # The Tauri updater requests darwin-<host-arch>, so the universal
        # updater bundle must be registered under BOTH update platforms.
        if [ -n "$TAR" ]; then
            upload "$TAR" --update-platform darwin-aarch64
            upload "$TAR" --update-platform darwin-x86_64 \
                || echo "    (warn) darwin-x86_64 registration failed — check the release's assets in the web UI" >&2
        fi
    else
        case "$ARCH" in
            arm64)  P=aarch64; UP=darwin-aarch64 ;;
            x86_64) P=x86_64;   UP=darwin-x86_64 ;;
        esac
        upload "$DMG" --public-platform "dmg-$P"
        [ -n "$TAR" ] && upload "$TAR" --update-platform "$UP"
    fi
elif [ "$PLATFORM" = "windows" ]; then
    # NSIS: the -setup.exe IS the updater bundle when a .sig exists.
    for f in "$BUNDLE_DIR"/nsis/*.exe; do
        [ -f "$f" ] || continue
        upload "$f" --public-platform nsis-x86_64
        [ -f "$f.sig" ] && upload "$f" --update-platform windows-x86_64
    done
else  # linux
    for f in "$BUNDLE_DIR"/appimage/*.AppImage; do
        [ -f "$f" ] || continue
        # AppImage is both the installer and the update bundle.
        UP_ARGS=()
        [ -f "$f.sig" ] && UP_ARGS=(--update-platform linux-x86_64)
        upload "$f" --public-platform appimage-x86_64 ${UP_ARGS[@]+"${UP_ARGS[@]}"}
    done
    for f in "$BUNDLE_DIR"/deb/*.deb; do
        [ -f "$f" ] || continue
        upload "$f" --public-platform deb-x86_64
    done
fi
fi  # DO_UPLOAD

if [ "$DO_PUBLISH" -eq 1 ]; then
    echo "==> Publishing"
    "$CN" release publish "$APP" "$VERSION" ${CHANNEL_ARGS[@]+"${CHANNEL_ARGS[@]}"}
    echo "==> Done: https://web.crabnebula.cloud/$APP/releases"
else
    echo "==> Draft kept unpublished (--no-publish): publish later with 'scripts/release.sh --publish' or the web UI"
fi
